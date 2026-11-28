import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { ModelCatalog, RemoteServerStatus, RemoteServerView } from "../ipc/types";
import { RemoteServersCard } from "./RemoteServers";
import { ToastProvider } from "./toast";

afterEach(cleanup);

globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

const ollama: RemoteServerView = {
  id: "ollama",
  name: "Ollama",
  base_url: "http://localhost:11434/v1",
  key: "none",
  models: [
    { id: "qwen3:8b", model_ref: "remote:ollama:qwen3:8b", embedding: false, tool_support: "supported", tool_support_reported: true },
    { id: "tiny", model_ref: "remote:ollama:tiny", embedding: false, tool_support: "unsupported", tool_support_reported: true },
    { id: "mystery", model_ref: "remote:ollama:mystery", embedding: false, tool_support: "unknown", tool_support_reported: false },
    { id: "nomic-embed-text", model_ref: "remote:ollama:nomic-embed-text", embedding: true, tool_support: "unsupported", tool_support_reported: true },
  ],
};

function fake(statuses: RemoteServerStatus[] = [{ state: "connected", models: 4 }]) {
  let servers: RemoteServerView[] = [];
  let removed: RemoteServerView | null = null;
  const calls: unknown[][] = [];
  const queue = [...statuses];
  const catalog = (): ModelCatalog => ({
    slots: [],
    models: [],
    providers: [],
    remote_servers: servers,
    local_models: [],
    key_store: "memory",
    keychain: { state: "ok" },
    settings: { slots: {}, telemetry_enabled: false },
    chat_model: null,
    chat_model_tools: false,
    compute: null,
  });
  const api = {
    addRemoteServer: async (name: string, url: string, key: string | null) => {
      calls.push(["add", name, url, key]);
      if (url.startsWith("http://api.")) {
        throw { code: "invalid", message: "api.example.com isn't on this computer or your local network, so it needs https://." };
      }
      servers = [ollama];
      return catalog();
    },
    refreshRemoteServer: async (id: string) => {
      calls.push(["refresh", id]);
      return catalog();
    },
    removeRemoteServer: async (id: string) => {
      calls.push(["remove", id]);
      removed = servers.find((s) => s.id === id) ?? null;
      servers = [];
      return catalog();
    },
    restoreRemoteServer: async (id: string) => {
      calls.push(["restore", id]);
      if (removed !== null) servers = [removed];
      return catalog();
    },
    modelContext: async () => ({ tokens: 32768, source: "provider" }) as const,
    checkRemoteServer: async (id: string) => {
      calls.push(["check", id]);
      return queue.length > 1 ? queue.shift()! : queue[0]!;
    },
    setRemoteModelTools: async (id: string, model: string, on: boolean) => {
      calls.push(["tools", id, model, on]);
      servers = [
        {
          ...ollama,
          models: ollama.models.map((m) => (m.id === model ? { ...m, tool_support: on ? "supported" : "unsupported" } : m)),
        },
      ];
      return catalog();
    },
  } as unknown as Api;
  return { api, calls, catalog, add: () => (servers = [ollama]) };
}

function Harness({ api, initial }: { api: Api; initial: ModelCatalog }) {
  const [c, setC] = useState(initial);
  return (
    <ToastProvider>
      <RemoteServersCard api={api} catalog={c} onCatalog={setC} />
    </ToastProvider>
  );
}

describe("RemoteServersCard", () => {
  it("starts empty, opens the form from Add model server, and shows tool support per model", async () => {
    const f = fake();
    render(<Harness api={f.api} initial={f.catalog()} />);
    expect(screen.getByText("No model servers yet")).toBeTruthy();
    expect(screen.queryByLabelText("Address")).toBeNull();
    const open = screen.getByRole("button", { name: "Add model server" });
    expect(open.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(open);
    expect(screen.getByRole("button", { name: "Add model server" }).getAttribute("aria-expanded")).toBe("true");

    // A remote http address is refused, with the reason under the field.
    fireEvent.change(screen.getByLabelText("Address"), { target: { value: "http://api.example.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(await screen.findByText(/needs https:\/\//)).toBeTruthy();

    fireEvent.change(screen.getByLabelText("Address"), { target: { value: "http://localhost:11434" } });
    fireEvent.change(screen.getByLabelText("Name (optional)"), { target: { value: "Ollama" } });
    fireEvent.change(screen.getByLabelText("API key (optional)"), { target: { value: "  " } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    const server = await screen.findByTestId("remote-server");
    expect(f.calls[1]).toEqual(["add", "Ollama", "http://localhost:11434", null]);
    // The form closes after adding; the server is checked once.
    await waitFor(() => expect(screen.queryByLabelText("Address")).toBeNull());
    expect(await within(server).findByText("Connected")).toBeTruthy();
    expect(f.calls.filter((c) => c[0] === "check")).toEqual([["check", "ollama"]]);
    expect(within(server).getByText(/http:\/\/localhost:11434\/v1 · 4 models/)).toBeTruthy();

    const row = (id: string) => server.querySelector(`[data-model="${id}"]`) as HTMLElement;
    expect(within(row("qwen3:8b")).getByText("Tool calls")).toBeTruthy();
    expect(within(row("tiny")).getByText("No tool calls")).toBeTruthy();
    expect(within(row("nomic-embed-text")).getByText("Embeddings")).toBeTruthy();
    // Only the model whose server doesn't say gets a switch, and it starts off.
    const sw = within(row("mystery")).getByRole("switch", { name: "mystery: tool calls" });
    expect(sw.getAttribute("aria-checked")).toBe("false");
    expect(server.querySelectorAll('[role="switch"]').length).toBe(1);
    fireEvent.click(sw);
    await waitFor(() => expect(f.calls.at(-1)).toEqual(["tools", "ollama", "mystery", true]));
    await waitFor(() =>
      expect(within(row("mystery")).getByRole("switch").getAttribute("aria-checked")).toBe("true"),
    );
    // The data-flow sentence is said once for the card, not per server or field.
    expect(screen.getAllByText(/are sent\s+to that server/).length).toBe(1);
  });

  it("shows Unreachable with Retry, and Needs key", async () => {
    const f = fake([
      { state: "unreachable", message: "Couldn't connect." },
      { state: "needs_key", message: "The server needs a key." },
    ]);
    f.add();
    render(<Harness api={f.api} initial={f.catalog()} />);
    expect(await screen.findByText("Unreachable")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Needs key")).toBeTruthy();
    expect(f.calls.filter((c) => c[0] === "check").length).toBe(2);
  });

  it("removes at once with Undo, and refreshes", async () => {
    const f = fake();
    f.add();
    render(<Harness api={f.api} initial={f.catalog()} />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh Ollama" }));
    await waitFor(() => expect(f.calls).toContainEqual(["refresh", "ollama"]));
    fireEvent.click(screen.getByRole("button", { name: "Remove Ollama" }));
    // No confirm dialog: it's gone, with Undo on the toast.
    await waitFor(() => expect(screen.queryByTestId("remote-server")).toBeNull());
    expect(screen.queryByRole("alertdialog")).toBeNull();
    const undo = await screen.findByRole("button", { name: "Undo" });
    await act(async () => {
      fireEvent.click(undo);
    });
    await waitFor(() => expect(f.calls).toContainEqual(["restore", "ollama"]));
    expect(await screen.findByTestId("remote-server")).toBeTruthy();
  });
});
