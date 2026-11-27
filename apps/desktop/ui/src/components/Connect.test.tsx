import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { ConnectEntryView, ConnectView, McpServerStatus } from "../ipc/types";
import { ConnectCard } from "./Connect";
import { TooltipProvider } from "./primitives";

afterEach(cleanup);

const entry = (over: Partial<ConnectEntryView> = {}): ConnectEntryView => ({
  id: "northwind",
  service: "Northwind Issues",
  vendor: "Northwind",
  summary: "Read and create issues and comments",
  url: "https://mcp.northwind.example/mcp",
  scopes: ["read", "write"],
  terms_url: "https://northwind.example/terms",
  data: "Goes between Northwind and this app; Demios's servers never see it.",
  review_date: "2026-11-26",
  review_source: "https://northwind.example/docs/mcp",
  reviewed: true,
  server_id: null,
  ...over,
});

const status = (state: McpServerStatus["state"], message: string | null = null): McpServerStatus => ({
  server_id: "northwind",
  name: "Northwind Issues",
  state,
  message,
  tools: state === "connected" ? 3 : 0,
  review: 0,
  revision: 0,
});

function fake(view: ConnectView | { error: string }) {
  let current = view;
  const calls: [string, unknown][] = [];
  let finishSignIn: ((ok: boolean) => void) | null = null;
  const api = {
    connectCatalogue: async () => {
      if ("error" in current) throw { code: "catalogue_unchecked", message: current.error };
      return current;
    },
    connectService: async (id: string) => {
      calls.push(["connect", id]);
      if (id === "broken") throw { code: "invalid_input", message: "You already have a tool server with the id broken." };
      current = { entries: (current as ConnectView).entries.map((e) => (e.id === id ? { ...e, server_id: id } : e)) };
      return current;
    },
    signInMcpServer: (id: string) => {
      calls.push(["signin", id]);
      return new Promise<null>((resolve, reject) => {
        finishSignIn = (ok) => (ok ? resolve(null) : reject({ code: "timeout", message: "Sign-in took too long. Try again." }));
      });
    },
    removeMcpServer: async (id: string) => {
      calls.push(["remove", id]);
      current = { entries: (current as ConnectView).entries.map((e) => (e.server_id === id ? { ...e, server_id: null } : e)) };
      return { servers: [], local_servers_allowed: true };
    },
    reconnectMcpServer: async (id: string) => (calls.push(["reconnect", id]), null),
    openLink: async (url: string) => (calls.push(["open", url]), true),
  } as unknown as Api;
  return { api, calls, finish: (ok: boolean) => finishSignIn?.(ok) };
}

const ui = (api: Api, live: McpServerStatus[] = [], onChanged = () => {}) =>
  render(
    <TooltipProvider>
      <ConnectCard api={api} live={live} version={0} onChanged={onChanged} />
    </TooltipProvider>,
  );

describe("ConnectCard", () => {
  it("lists each service with who runs it, and Details says where data goes", async () => {
    const { api, calls } = fake({ entries: [entry()] });
    ui(api);
    const row = await screen.findByTestId("connect-row");
    expect(within(row).getByText("Northwind Issues")).toBeTruthy();
    expect(within(row).getByText("Run by Northwind · Sign in with Northwind · Reviewed 26 Nov 2026")).toBeTruthy();
    const details = within(row).getByRole("button", { name: "Details for Northwind Issues" });
    expect(details.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(details);
    expect(details.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("https://mcp.northwind.example/mcp")).toBeTruthy();
    expect(screen.getByText(/Demios's servers never see it/)).toBeTruthy();
    expect(screen.getByText("read · write")).toBeTruthy();
    expect(screen.getByText(/Reads ask once per chat; anything that creates, changes or deletes asks every time/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Northwind's terms" }));
    await waitFor(() => expect(calls).toContainEqual(["open", "https://northwind.example/terms"]));
  });

  it("Connect adds the server, then signs in; Cancel removes it again", async () => {
    const { api, calls } = fake({ entries: [entry()] });
    let changed = 0;
    ui(api, [], () => changed++);
    fireEvent.click(await screen.findByRole("button", { name: "Connect Northwind Issues" }));
    await waitFor(() => expect(calls).toContainEqual(["signin", "northwind"]));
    expect(calls[0]).toEqual(["connect", "northwind"]);
    expect(changed).toBe(1);
    expect(screen.getByText("Finish signing in to Northwind in your browser.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel connecting Northwind Issues" }));
    await waitFor(() => expect(calls).toContainEqual(["remove", "northwind"]));
    await waitFor(() => expect(changed).toBe(2));
  });

  it("a cancelled sign-in that fails later shows nothing", async () => {
    const { api, calls, finish } = fake({ entries: [entry()] });
    ui(api);
    fireEvent.click(await screen.findByRole("button", { name: "Connect Northwind Issues" }));
    await waitFor(() => expect(calls).toContainEqual(["signin", "northwind"]));
    fireEvent.click(screen.getByRole("button", { name: "Cancel connecting Northwind Issues" }));
    await waitFor(() => expect(calls).toContainEqual(["remove", "northwind"]));
    finish(false);
    expect(await screen.findByRole("button", { name: "Connect Northwind Issues" })).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("a failed sign-in says why, with Try again; nothing else is used instead", async () => {
    const { api, calls, finish } = fake({ entries: [entry()] });
    ui(api);
    fireEvent.click(await screen.findByRole("button", { name: "Connect Northwind Issues" }));
    await waitFor(() => expect(calls).toContainEqual(["signin", "northwind"]));
    finish(false);
    expect((await screen.findByRole("alert")).textContent).toBe("Couldn't add Northwind Issues: Sign-in took too long. Try again.");
    fireEvent.click(screen.getByRole("button", { name: "Try connecting Northwind Issues again" }));
    // Already added: only signs in again, never adds a second server.
    await waitFor(() => expect(calls.filter((c) => c[0] === "signin")).toHaveLength(2));
    expect(calls.filter((c) => c[0] === "connect")).toHaveLength(1);
  });

  it("an add the app refuses shows the reason", async () => {
    const { api } = fake({ entries: [entry({ id: "broken", service: "Broken" })] });
    ui(api);
    fireEvent.click(await screen.findByRole("button", { name: "Connect Broken" }));
    expect((await screen.findByRole("alert")).textContent).toContain("You already have a tool server with the id broken.");
  });

  it("shows connected, needs sign-in and failed from the server's live status", async () => {
    const added = { entries: [entry({ server_id: "northwind" })] };
    const { api: a1 } = fake(added);
    const first = ui(a1, [status("connected")]);
    const row = await screen.findByTestId("connect-row");
    expect(within(row).getByText("Connected")).toBeTruthy();
    expect(within(row).getByRole("button", { name: "Show Northwind Issues in tool servers" })).toBeTruthy();
    first.unmount();

    const { api: a2, calls } = fake(added);
    const second = ui(a2, [status("needs_auth", "Sign in to use this server's tools.")]);
    fireEvent.click(await screen.findByRole("button", { name: "Sign in to Northwind Issues" }));
    await waitFor(() => expect(calls).toContainEqual(["signin", "northwind"]));
    second.unmount();

    const { api: a3, calls: c3 } = fake(added);
    ui(a3, [status("failed", "Couldn't reach the server.")]);
    expect((await screen.findByRole("alert")).textContent).toBe("Couldn't reach the server.");
    fireEvent.click(screen.getByRole("button", { name: "Try connecting Northwind Issues again" }));
    await waitFor(() => expect(c3).toContainEqual(["reconnect", "northwind"]));
  });

  it("a server no longer reviewed is marked and offered for removal", async () => {
    const { api, calls } = fake({ entries: [entry({ server_id: "northwind", reviewed: false })] });
    ui(api);
    const row = await screen.findByTestId("connect-row");
    expect(within(row).getByText("No longer reviewed")).toBeTruthy();
    expect(within(row).queryByRole("button", { name: "Connect Northwind Issues" })).toBeNull();
    fireEvent.click(within(row).getByRole("button", { name: "Remove Northwind Issues" }));
    await waitFor(() => expect(calls).toContainEqual(["remove", "northwind"]));
  });

  it("a catalogue that can't be checked shows no list at all", async () => {
    const msg = "The list of services couldn't be checked, so it isn't shown. Reinstall Demios to get a good copy.";
    const { api } = fake({ error: msg });
    ui(api);
    expect((await screen.findByRole("alert")).textContent).toContain(msg);
    expect(screen.queryByTestId("connect-row")).toBeNull();
  });
});
