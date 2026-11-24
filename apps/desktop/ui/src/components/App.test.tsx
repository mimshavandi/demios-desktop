import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { ChatEvent, Conversation, ModelCatalog, Settings, StoredMessage } from "../ipc/types";
import { App } from "./App";

afterEach(cleanup);

/** In-memory stand-in for the Rust commands. */
function fakeApi() {
  const convs: Conversation[] = [];
  const trash: Conversation[] = [];
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
      const i = convs.findIndex((c) => c.id === cid);
      const [c] = convs.splice(i, 1);
      if (c) trash.push(c);
      return null;
    },
    restoreConversation: async (cid) => {
      const i = trash.findIndex((c) => c.id === cid);
      const [c] = trash.splice(i, 1);
      if (c) convs.unshift(c);
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
    restoreProviderKey: async () => catalog(),
  };
  return { api, emit: (ev: ChatEvent) => listener?.(ev), msgs };
}

describe("App", () => {
  it("sends a message and renders the streamed answer as markdown", async () => {
    const { api, emit } = fakeApi();
    render(<App api={api} />);
    expect(await screen.findByText("What can I help with?")).toBeTruthy();
    expect(screen.getByTestId("artifact-pane")).toBeTruthy();
    expect(screen.getByText("No chats yet")).toBeTruthy();

    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "Hello" } });
    fireEvent.keyDown(box, { key: "Enter" });

    expect(await screen.findByRole("button", { name: "Stop answer" })).toBeTruthy();
    const assistantId = "id-3";
    act(() => {
      emit({ type: "text_delta", message_id: assistantId, offset: 0, text: "Hi **there**" });
    });
    expect((await screen.findByText("there")).tagName).toBe("STRONG");
    act(() => {
      emit({ type: "done", message_id: assistantId, finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 2 } });
    });
    expect(await screen.findByRole("button", { name: "Retry" })).toBeTruthy();
    await waitFor(() => expect(screen.getByRole("button", { name: "Hello" })).toBeTruthy());
    // The model badge says where the answer ran.
    expect(screen.getAllByText(/On this device/).length).toBeGreaterThan(0);
  });

  it("keeps the prompt when sending fails", async () => {
    const { api } = fakeApi();
    api.sendMessage = async () => {
      throw { code: "busy", message: "An answer is already streaming in this chat." };
    };
    render(<App api={api} />);
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "keep me" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(await screen.findByText("An answer is already streaming in this chat.")).toBeTruthy();
    expect(box.value).toBe("keep me");
  });

  it("deletes a chat at once and restores it with undo", async () => {
    const { api } = fakeApi();
    await api.createConversation();
    let deletes = 0;
    let restores = 0;
    const del = api.deleteConversation;
    const res = api.restoreConversation;
    api.deleteConversation = async (id) => {
      deletes++;
      return del(id);
    };
    api.restoreConversation = async (id) => {
      restores++;
      return res(id);
    };
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Delete “New chat”" }));
    expect(await screen.findByText("Deleted “New chat”")).toBeTruthy();
    expect(deletes).toBe(1);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Delete “New chat”" })).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(await screen.findByRole("button", { name: "Delete “New chat”" })).toBeTruthy();
    expect(restores).toBe(1);
    expect(deletes).toBe(1);
  });

  it("shows models and settings with telemetry off", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Models" }));
    expect(await screen.findByText("Model for each task")).toBeTruthy();
    expect(screen.getByLabelText("Chat")).toBeTruthy();
    const key = screen.getByLabelText("OpenAI") as HTMLInputElement;
    expect(key.type).toBe("password");
    fireEvent.click(screen.getByRole("button", { name: "Save key" }));
    expect(await screen.findByText("Paste the key first.")).toBeTruthy();
    expect(key.getAttribute("aria-invalid")).toBe("true");

    fireEvent.click(screen.getByRole("tab", { name: "General" }));
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe("tab-general");
    const telemetry = await screen.findByRole("switch", { name: "Share anonymous usage statistics" });
    expect(telemetry.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(telemetry);
    expect(await screen.findByText("Usage statistics setting saved")).toBeTruthy();
  });

  it("asks to connect a model when none is available", async () => {
    const { api } = fakeApi();
    api.appInfo = async () => ({ version: "0.1.0", dev_fake_model: false, data_dir: null });
    render(<App api={api} />);
    expect(await screen.findByText(/No model connected/)).toBeTruthy();
    expect((screen.getByLabelText("Message") as HTMLTextAreaElement).disabled).toBe(true);
    expect(screen.getAllByRole("button", { name: "Connect a model" }).length).toBeGreaterThan(0);
  });
});
