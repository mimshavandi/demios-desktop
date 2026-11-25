import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { UpdateCheck, UpdateMode, UpdateProgress } from "../ipc/types";
import { TooltipProvider } from "./primitives";
import { Updates } from "./Updates";

afterEach(cleanup);

const available = (patch: Partial<Extract<UpdateCheck, { status: "available" }>> = {}): UpdateCheck => ({
  status: "available",
  version: "0.2.0",
  current: "0.1.0",
  notes: "Added\n- Check for updates",
  date: "2027-01-15T10:00:00Z",
  ...patch,
});

function fake(check: () => Promise<UpdateCheck>, install?: (v: string, p: (e: UpdateProgress) => void) => Promise<null>) {
  const calls: string[] = [];
  const api = {
    checkForUpdate: async () => {
      calls.push("check");
      return check();
    },
    installUpdate: async (v: string, p: (e: UpdateProgress) => void) => {
      calls.push(`install ${v}`);
      return install ? install(v, p) : new Promise<null>(() => {});
    },
  } as unknown as Api;
  return { api, calls };
}

const ui = (api: Api, mode: UpdateMode = "self_update", online = () => true) =>
  render(
    <TooltipProvider>
      <Updates api={api} mode={mode} online={online} />
    </TooltipProvider>,
  );

const ipcError = (code: string, message: string) => ({ code, message });

describe("Updates (T-STORE-08)", () => {
  it("store, Flathub and builds without the updater say who updates them, with nothing to click", () => {
    const { api, calls } = fake(async () => available());
    for (const [mode, text] of [
      ["microsoft_store", /Microsoft Store keeps/],
      ["flathub", /Flathub keeps/],
      ["off", /doesn't update itself/],
      ["no_key", /no update key/],
    ] as const) {
      ui(api, mode);
      const row = screen.getByTestId("updates");
      expect(within(row).getByText(text)).toBeTruthy();
      expect(within(row).queryByRole("button")).toBeNull();
      cleanup();
    }
    expect(calls).toEqual([]);
  });

  it("checks only when asked, shows it is checking, then that it is up to date", async () => {
    let resolve: (c: UpdateCheck) => void = () => {};
    const { api, calls } = fake(() => new Promise((r) => (resolve = r)));
    ui(api);
    expect(calls).toEqual([]);
    expect(screen.getByText(/Nothing downloads until you choose to install it/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText("Checking for updates…")).toBeTruthy();
    expect((screen.getByRole("button", { name: /Check for updates/ }) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => resolve({ status: "up_to_date", current: "0.1.0" }));
    expect(await screen.findByText(/Demios 0.1.0 is the latest version/)).toBeTruthy();
    expect(calls).toEqual(["check"]);
  });

  it("shows a newer version with its notes as plain text, and downloads nothing yet", async () => {
    const hostile = '<img src=x onerror="window.__pwned=1"> **bold** [link](https://evil.example)';
    const { api, calls } = fake(async () => available({ notes: hostile }));
    ui(api);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText(/Version 0.2.0 is available. You have 0.1.0./)).toBeTruthy();
    const notes = screen.getByRole("region", { name: "What's new in 0.2.0" });
    expect(notes.textContent).toBe(hostile);
    expect(notes.querySelector("img, a, b, strong")).toBeNull();
    expect((window as { __pwned?: number }).__pwned).toBeUndefined();
    expect(screen.getByRole("button", { name: "Install and restart" })).toBeTruthy();
    expect(calls).toEqual(["check"]);
  });

  it("installs only after the user confirms, then shows progress", async () => {
    let send: (p: UpdateProgress) => void = () => {};
    const { api, calls } = fake(
      async () => available(),
      (_, p) => {
        send = p;
        return new Promise<null>(() => {});
      },
    );
    ui(api);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    const install = await screen.findByRole("button", { name: "Install and restart" });

    // "Not now" changes nothing and returns focus.
    fireEvent.click(install);
    let dialog = await screen.findByRole("alertdialog", { name: "Install Demios 0.2.0 and restart?" });
    expect(dialog.textContent).toContain("checks that it is signed");
    const notNow = within(dialog).getByRole("button", { name: "Not now" });
    expect(document.activeElement).toBe(notNow);
    fireEvent.click(notNow);
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(install));
    expect(calls).toEqual(["check"]);

    fireEvent.click(install);
    dialog = await screen.findByRole("alertdialog");
    const confirm = within(dialog).getByRole("button", { name: "Install and restart" }) as HTMLButtonElement;
    await waitFor(() => expect(confirm.disabled).toBe(false), { timeout: 1500 });
    fireEvent.click(confirm);
    await waitFor(() => expect(calls).toEqual(["check", "install 0.2.0"]));
    expect(await screen.findByText("Downloading the update…")).toBeTruthy();

    act(() => send({ kind: "downloading", downloaded: 10_000_000, total: 40_000_000 }));
    const bar = screen.getByRole("progressbar", { name: "Updates" });
    expect(bar.getAttribute("aria-valuenow")).toBe("10000000");
    expect(bar.getAttribute("aria-valuetext")).toBe("Downloading 10 MB of 40 MB");
    act(() => send({ kind: "installing" }));
    expect(screen.getByText(/Installing… Demios restarts when it's done/)).toBeTruthy();
  });

  it("shows an error and lets the user try again", async () => {
    let fail = true;
    const { api, calls } = fake(async () => {
      if (fail) throw ipcError("update_unreachable", "Couldn't get the update information from GitHub. Try again later.");
      return { status: "up_to_date", current: "0.1.0" };
    });
    ui(api);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Couldn't check for updates. Couldn't get the update information from GitHub.");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText(/is the latest version/)).toBeTruthy();
    expect(calls).toEqual(["check", "check"]);
  });

  it("says when the computer is offline, without asking GitHub", async () => {
    const { api, calls } = fake(async () => available());
    ui(api, "self_update", () => false);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText(/You're offline/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(calls).toEqual([]);
  });

  it("treats a connection failure from the backend as offline", async () => {
    const { api } = fake(async () => {
      throw ipcError("update_offline", "Couldn't reach GitHub.");
    });
    ui(api);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText(/You're offline/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
  });

  it("keeps the update on offer when installing fails", async () => {
    const { api } = fake(
      async () => available(),
      async () => {
        throw ipcError("update_signature", "The download didn't match the Demios signature, so it wasn't installed.");
      },
    );
    ui(api);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    fireEvent.click(await screen.findByRole("button", { name: "Install and restart" }));
    const dialog = await screen.findByRole("alertdialog");
    const confirm = within(dialog).getByRole("button", { name: "Install and restart" }) as HTMLButtonElement;
    await waitFor(() => expect(confirm.disabled).toBe(false), { timeout: 1500 });
    fireEvent.click(confirm);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("didn't match the Demios signature");
    expect(screen.getByRole("button", { name: "Install and restart" })).toBeTruthy();
  });
});
