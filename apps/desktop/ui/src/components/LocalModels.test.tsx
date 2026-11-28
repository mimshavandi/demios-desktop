// Remove with Undo and Cancel download on the On this device rows (#176).
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { DownloadState, LocalModelEvent, LocalModelRow } from "../ipc/types";
import { LocalModelsCard } from "./LocalModels";
import { TooltipProvider } from "./primitives";
import { ToastProvider } from "./toast";

afterEach(cleanup);

function row(state: DownloadState, over: Partial<LocalModelRow> = {}): LocalModelRow {
  return {
    id: "qwen3-8b",
    origin: "catalog",
    location: null,
    model_ref: "local:qwen3-8b",
    name: "Qwen3 8B",
    repo: "Qwen/Qwen3-8B-GGUF",
    quant: "Q4_K_M",
    size: 5_027_783_488,
    params_b: 8.2,
    capabilities: ["chat"],
    license: "apache-2.0",
    license_warning: false,
    card_url: "https://huggingface.co/Qwen/Qwen3-8B-GGUF",
    notes: "",
    state,
    fit: { status: "ready", estimated: false, reason: "Fits.", suggested_quant: null },
    traits: null,
    remove_blocked: null,
    ...over,
  };
}

/** A backend for one row: records calls and sends row updates like the app does. */
function fake(initial: LocalModelRow) {
  let sink: ((e: LocalModelEvent) => void) | null = null;
  let current = initial;
  const calls: string[] = [];
  const send = (r: LocalModelRow) => {
    current = r;
    act(() => sink?.({ type: "changed", row: r }));
  };
  const api = {
    watchLocalModels: async (on: (e: LocalModelEvent) => void) => ((sink = on), [current]),
    removeLocalModel: async (id: string) => {
      calls.push(`remove ${id}`);
      send({ ...current, removing: true });
      return null;
    },
    undoRemoveLocalModel: async (id: string) => {
      calls.push(`undo ${id}`);
      send({ ...current, removing: false });
      return null;
    },
    cancelDownload: async (id: string) => {
      calls.push(`cancel ${id}`);
      send({ ...current, state: { state: "not_downloaded" } });
      return null;
    },
  } as unknown as Api;
  return { api, calls, send };
}

let changed = 0;
const ui = (api: Api, r: LocalModelRow) =>
  render(
    <TooltipProvider>
      <ToastProvider>
        <LocalModelsCard api={api} initial={[r]} onReady={() => (changed += 1)} />
      </ToastProvider>
    </TooltipProvider>,
  );

describe("On this device: Remove and Cancel download (#176)", () => {
  it("removes at once with Undo on the toast and on the row; Undo brings it back", async () => {
    const r = row({ state: "ready" });
    const { api, calls } = fake(r);
    ui(api, r);
    changed = 0;
    fireEvent.click(await screen.findByRole("button", { name: "Remove Qwen3 8B (5.0 GB)" }));
    await screen.findByText("Qwen3 8B removed (5.0 GB)");
    expect(calls).toEqual(["remove qwen3-8b"]);
    expect(changed).toBe(1);
    // The row says so and offers Undo too; Remove and Set as default are gone meanwhile.
    expect(await screen.findByText("Removed. The file is deleted in a few seconds.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Remove Qwen3 8B/ })).toBeNull();
    // The toast's Undo (the row's is "Undo removing Qwen3 8B").
    expect(screen.getByRole("button", { name: "Undo removing Qwen3 8B" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await screen.findByText("Qwen3 8B restored");
    expect(calls).toEqual(["remove qwen3-8b", "undo qwen3-8b"]);
    expect(changed).toBe(2);
    expect(await screen.findByRole("button", { name: "Remove Qwen3 8B (5.0 GB)" })).toBeTruthy();
  });

  it("Undo on the row works without the toast", async () => {
    const r = row({ state: "ready" }, { removing: true });
    const { api, calls } = fake(r);
    ui(api, r);
    fireEvent.click(await screen.findByRole("button", { name: "Undo removing Qwen3 8B" }));
    await screen.findByText("Qwen3 8B restored");
    expect(calls).toEqual(["undo qwen3-8b"]);
  });

  it("after the undo window the row is not downloaded, and a failed removal says why", async () => {
    const r = row({ state: "ready" }, { removing: true });
    const { api, send } = fake(r);
    ui(api, r);
    await screen.findByText("Removed. The file is deleted in a few seconds.");
    send({ ...r, removing: false, state: { state: "not_downloaded" } });
    expect(await screen.findByRole("button", { name: "Download model" })).toBeTruthy();
    send({ ...r, removing: false, remove_error: "Qwen3 8B is answering. Try again when it finishes." });
    expect(await screen.findByText(/Couldn't remove Qwen3 8B\. Qwen3 8B is answering/)).toBeTruthy();
  });

  it("a model Almend may not delete shows Remove disabled with the reason", async () => {
    const reason = "This model is in a folder you chose (/models). Almend doesn't delete files there; remove it in your file manager.";
    const r = row({ state: "ready" }, { remove_blocked: reason });
    const { api } = fake(r);
    ui(api, r);
    const btn = (await screen.findByRole("button", { name: "Remove Qwen3 8B (5.0 GB)" })) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    expect(document.getElementById(btn.getAttribute("aria-describedby") ?? "")?.textContent).toBe(reason);
  });

  it.each([
    ["downloading", { state: "downloading", done: 1e9, total: 5e9, bytes_per_sec: 1e7 }],
    ["paused", { state: "paused", done: 1e9, total: 5e9 }],
  ] as const)("cancels a %s download", async (_, state) => {
    const r = row(state as DownloadState);
    const { api, calls } = fake(r);
    ui(api, r);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel download of Qwen3 8B" }));
    await screen.findByText("Download of Qwen3 8B cancelled");
    expect(calls).toEqual(["cancel qwen3-8b"]);
    await waitFor(() => expect(screen.getByRole("button", { name: "Download model" })).toBeTruthy());
  });
});
