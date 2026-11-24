import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { ChatEvent, Conversation, ModelCatalog, Settings, StoredMessage } from "../ipc/types";
import { App } from "./App";

afterEach(cleanup);

/** In-memory stand-in for the Rust commands. */
function fakeApi() {
  const convs: Conversation[] = [];
  const msgs = new Map<string, StoredMessage[]>();
  let settings: Settings = { slots: {}, telemetry_enabled: false };
  let n = 0;
  let listener: ((ev: ChatEvent) => void) | null = null;
  const id = () => `id-${++n}`;
  const m = (cid: string, role: "user" | "assistant", text: string, status: StoredMessage["status"]): StoredMessage => ({
    id: id(),
    conversation_id: cid,
    role,
    content: text === "" ? [] : [{ type: "text", text }],
    tool_calls: [],
    tool_call_id: null,
    model: "local:dev-fake",
    status,
    finish_reason: null,
    error: null,
    created_at: n,
    updated_at: n,
  });
  const catalog = (): ModelCatalog => ({
    slots: [{ slot_id: "chat", label: "Chat", description: "Answers", capability: "chat" }],
    models: [
      { model_ref: "local:dev-fake", label: "Dev fake", source: "dev", capabilities: ["chat"], available: true, note: null },
    ],
    providers: [{ id: "openai", name: "OpenAI", has_key: false }],
    local_models: [],
    key_store: "memory",
    settings,
  });
  const api: Api = {
    appInfo: async () => ({ version: "0.1.0", dev_fake_model: true, data_dir: "/tmp" }),
    listConversations: async () => [...convs],
    createConversation: async () => {
      const c = { id: id(), title: "New chat", model: null, created_at: n, updated_at: n };
      convs.unshift(c);
      msgs.set(c.id, []);
      return c;
    },
    renameConversation: async () => null,
    deleteConversation: async (cid) => {
      convs.splice(convs.findIndex((c) => c.id === cid), 1);
      return null;
    },
    listMessages: async (cid) => [...(msgs.get(cid) ?? [])],
    sendMessage: async (cid, text, onEvent) => {
      listener = onEvent;
      const u = m(cid, "user", text, "complete");
      const a = m(cid, "assistant", "", "streaming");
      msgs.get(cid)?.push(u, a);
      const c = convs.find((x) => x.id === cid);
      if (c) c.title = text;
      return { user_message: u, assistant_message: a, title: text };
    },
    regenerate: async () => {
      throw { code: "invalid_input", message: "Nothing to regenerate." };
    },
    stopGeneration: async () => true,
    getSettings: async () => settings,
    updateSettings: async (s) => {
      settings = s;
      return s;
    },
    listModels: async () => catalog(),
    setProviderKey: async () => catalog(),
    deleteProviderKey: async () => catalog(),
  };
  return { api, emit: (ev: ChatEvent) => listener?.(ev), msgs };
}

describe("App", () => {
  it("sends a message and renders the streamed answer as markdown", async () => {
    const { api, emit } = fakeApi();
    render(<App api={api} />);
    expect(await screen.findByText("What can I help with?")).toBeTruthy();
    expect(screen.getByTestId("artifact-pane")).toBeTruthy();

    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "Hello" } });
    fireEvent.keyDown(box, { key: "Enter" });

    const stop = await screen.findByRole("button", { name: "Stop" });
    expect(stop).toBeTruthy();
    const assistantId = "id-3";
    act(() => {
      emit({ type: "text_delta", message_id: assistantId, text: "Hi **there**" });
    });
    expect((await screen.findByText("there")).tagName).toBe("STRONG");
    act(() => {
      emit({ type: "done", message_id: assistantId, finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 2 } });
    });
    expect(await screen.findByRole("button", { name: "Regenerate" })).toBeTruthy();
    await waitFor(() => expect(screen.getByRole("button", { name: "Hello" })).toBeTruthy());
  });

  it("opens settings with telemetry off and per-slot model choice", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
    const telemetry = (await screen.findByLabelText(/usage statistics/)) as HTMLInputElement;
    expect(telemetry.checked).toBe(false);
    const slot = screen.getByLabelText("Chat") as HTMLSelectElement;
    fireEvent.change(slot, { target: { value: "local:dev-fake" } });
    expect(await screen.findByText("Chat model saved.")).toBeTruthy();
    expect(screen.getByLabelText("OpenAI")).toHaveProperty("type", "password");
  });
});
