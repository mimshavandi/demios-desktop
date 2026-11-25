import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { ModelCatalog, RemoteServerView } from "../ipc/types";
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
  has_key: false,
  models: [
    { id: "qwen3:8b", model_ref: "remote:ollama:qwen3:8b", embedding: false, tool_support: "supported", tool_support_reported: true },
    { id: "tiny", model_ref: "remote:ollama:tiny", embedding: false, tool_support: "unsupported", tool_support_reported: true },
    { id: "mystery", model_ref: "remote:ollama:mystery", embedding: false, tool_support: "unknown", tool_support_reported: false },
    { id: "nomic-embed-text", model_ref: "remote:ollama:nomic-embed-text", embedding: true, tool_support: "unsupported", tool_support_reported: true },
  ],
};

function fake() {
  let servers: RemoteServerView[] = [];
  const calls: unknown[][] = [];
  const catalog = (): ModelCatalog => ({
    slots: [],
    models: [],
    providers: [],
    remote_servers: servers,
    local_models: [],
    key_store: "memory",
    settings: { slots: {}, telemetry_enabled: false },
    chat_model: null,
    chat_model_tools: false,
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
      servers = [];
      return catalog();
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
  return { api, calls, catalog };
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
  it("adds a server and shows what each model says about tool calls", async () => {
    const f = fake();
    render(<Harness api={f.api} initial={f.catalog()} />);
    // A remote http address is refused, with the reason under the field.
    fireEvent.change(screen.getByLabelText("Address"), { target: { value: "http://api.example.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Add server" }));
    expect(await screen.findByText(/needs https:\/\//)).toBeTruthy();

    fireEvent.change(screen.getByLabelText("Address"), { target: { value: "http://localhost:11434" } });
    fireEvent.change(screen.getByLabelText("Name (optional)"), { target: { value: "Ollama" } });
    fireEvent.change(screen.getByLabelText("API key (optional)"), { target: { value: "  " } });
    fireEvent.click(screen.getByRole("button", { name: "Add server" }));
    const server = await screen.findByTestId("remote-server");
    expect(f.calls[1]).toEqual(["add", "Ollama", "http://localhost:11434", null]);
    expect(within(server).getByText(/http:\/\/localhost:11434\/v1 · no key · 4 models/)).toBeTruthy();

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
  });

  it("refreshes and removes a server after confirming", async () => {
    const f = fake();
    await f.api.addRemoteServer("Ollama", "http://localhost:11434", null);
    render(<Harness api={f.api} initial={f.catalog()} />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(f.calls.at(-1)).toEqual(["refresh", "ollama"]));
    fireEvent.click(screen.getByRole("button", { name: "Remove Ollama" }));
    const confirm = (await screen.findByRole("button", { name: "Remove server" })) as HTMLButtonElement;
    await waitFor(() => expect(confirm.disabled).toBe(false), { timeout: 2000 });
    fireEvent.click(confirm);
    await waitFor(() => expect(f.calls.at(-1)).toEqual(["remove", "ollama"]));
    await waitFor(() => expect(screen.queryByTestId("remote-server")).toBeNull());
  });
});
