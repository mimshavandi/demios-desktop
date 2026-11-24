import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type {
  ApprovalPrompt,
  ChatEvent,
  Conversation,
  ModelCatalog,
  Settings,
  StoredMessage,
  ToolEvent,
  UnknownOutcome,
} from "../ipc/types";
import { App } from "./App";

afterEach(cleanup);

/** In-memory stand-in for the Rust commands. */
function fakeApi() {
  const convs: Conversation[] = [];
  const trash: Conversation[] = [];
  let approvalSink: ((e: ToolEvent) => void) | null = null;
  const responses: [string, string][] = [];
  const resolved: [string, boolean][] = [];
  const unknowns: UnknownOutcome[] = [];
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
    chat_model: null,
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
    watchTools: async (onEvent) => {
      approvalSink = onEvent;
      return { approvals: [], servers: [] };
    },
    respondApproval: async (requestId, decision) => {
      responses.push([requestId, decision]);
      return null;
    },
    listUnknownOutcomes: async () => [...unknowns],
    resolveUnknownOutcome: async (key, happened) => {
      resolved.push([key, happened]);
      unknowns.splice(0);
      return null;
    },
  };
  return {
    api,
    emit: (ev: ChatEvent) => listener?.(ev),
    msgs,
    approve: (e: ToolEvent) => approvalSink?.(e),
    responses,
    resolved,
    unknowns,
  };
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

  it("shows an approval card with the exact arguments and never auto-allows", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    const prompt = (risk: ApprovalPrompt["risk"]): ApprovalPrompt => ({
      request_id: `r-${risk}`,
      scope: `conv:${c.id}`,
      capability_id: "mcp.files.write_file",
      title: "Write to notes/todo.md",
      description: "Replaces the file.",
      risk,
      policy: "ask_once_per_conversation",
      arguments: { path: "notes/todo.md", content: "milk" },
    });
    act(() => f.approve({ type: "requested", prompt: prompt("low_write") }));
    const card = await screen.findByTestId("approval-card");
    expect(card.textContent).toContain('"path": "notes/todo.md"');
    expect(screen.getByText("Changes local data")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Always allow in this chat/ })).toBeTruthy();
    expect(f.responses).toEqual([]);
    // Keys in the first 500 ms after the card appears are ignored.
    fireEvent.keyDown(card, { key: "Enter", metaKey: true, ctrlKey: true });
    expect(f.responses).toEqual([]);
    await act(() => new Promise((r) => setTimeout(r, 550)));
    // ⌘↵ / Ctrl+↵ allows once.
    fireEvent.keyDown(card, { key: "Enter", metaKey: true, ctrlKey: true });
    await waitFor(() => expect(f.responses).toEqual([["r-low_write", "approve"]]));
    await waitFor(() => expect(screen.queryByTestId("approval-card")).toBeNull());

    // Destructive: no "always allow"; Esc denies.
    act(() => f.approve({ type: "requested", prompt: prompt("destructive") }));
    const card2 = await screen.findByTestId("approval-card");
    expect(screen.queryByRole("button", { name: /Always allow in this chat/ })).toBeNull();
    await act(() => new Promise((r) => setTimeout(r, 550)));
    fireEvent.keyDown(card2, { key: "Escape" });
    await waitFor(() => expect(f.responses[1]).toEqual(["r-destructive", "deny"]));
  });

  it("asks the user to resolve an unknown outcome", async () => {
    const f = fakeApi();
    await f.api.createConversation();
    f.unknowns.push({
      key: "k1",
      capability_id: "mcp.mail.send",
      arguments: { to: "pat@example.com" },
      error: "interrupted",
      updated_at_ms: 1,
    });
    render(<App api={f.api} />);
    expect(await screen.findByText("Did this go through?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "It didn't happen" }));
    await waitFor(() => expect(f.resolved).toEqual([["k1", false]]));
    await waitFor(() => expect(screen.queryByText("Did this go through?")).toBeNull());
  });

  it("renders stored tool calls as cards with their results", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    const base = {
      conversation_id: c.id,
      tool_call_id: null,
      model: "byok:anthropic:claude-sonnet-5-5",
      status: "complete" as const,
      error: null,
      created_at: 1,
      updated_at: 1,
    };
    f.msgs.set(c.id, [
      { ...base, id: "u", role: "user", content: [{ type: "text", text: "save it" }], tool_calls: [], finish_reason: null },
      {
        ...base,
        id: "a1",
        role: "assistant",
        content: [],
        tool_calls: [{ id: "call_1", name: "mcp__files__write_file", arguments: { path: "x.md" } }],
        finish_reason: "tool_calls",
      },
      { ...base, id: "t", role: "tool", tool_call_id: "call_1", content: [{ type: "text", text: "Wrote 1 file." }], tool_calls: [], finish_reason: null },
      { ...base, id: "a2", role: "assistant", content: [{ type: "text", text: "Saved." }], tool_calls: [], finish_reason: "stop" },
    ]);
    render(<App api={f.api} />);
    const card = await screen.findByTestId("tool-call-card");
    expect(card.textContent).toContain("files · write_file");
    expect(card.textContent).toContain("Done");
    expect(screen.queryByText("Wrote 1 file.")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /write_file/ }));
    expect(await screen.findByText("Wrote 1 file.")).toBeTruthy();
    expect(screen.getByText("Saved.")).toBeTruthy();
  });

  it("never approves when the user is typing in the composer as a card appears", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    box.focus();
    fireEvent.change(box, { target: { value: "next message" } });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r1",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write to notes/todo.md",
          description: "",
          risk: "low_write",
          policy: "ask_once_per_conversation",
          arguments: { path: "notes/todo.md" },
        },
      }),
    );
    await screen.findByTestId("approval-card");
    expect(document.activeElement).toBe(box);
    fireEvent.keyDown(box, { key: "Enter", metaKey: true, ctrlKey: true });
    await act(() => new Promise((r) => setTimeout(r, 50)));
    expect(f.responses).toEqual([]);
    expect(screen.getByTestId("approval-card")).toBeTruthy();
    expect(await screen.findByText("Approval needed: Write to notes/todo.md")).toBeTruthy();
  });

  it("tells the user when an approval expired and when a tool server failed", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r2",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write",
          description: "",
          risk: "low_write",
          policy: "always_ask",
          arguments: {},
        },
      }),
    );
    await screen.findByTestId("approval-card");
    act(() => f.approve({ type: "resolved", request_id: "r2", expired: true }));
    expect(await screen.findByText(/approval request expired/)).toBeTruthy();
    expect(screen.queryByTestId("approval-card")).toBeNull();

    act(() =>
      f.approve({
        type: "mcp_server",
        status: { server_id: "fs", name: "Files", state: "failed", message: "It didn't start.", tools: 0 },
      }),
    );
    expect(await screen.findByText("Files couldn't connect. It didn't start.")).toBeTruthy();
  });

  it("ignores clicks on a card's buttons in its first half second", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r3",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write",
          description: "",
          risk: "low_write",
          policy: "always_ask",
          arguments: {},
        },
      }),
    );
    const allow = await screen.findByRole("button", { name: /Allow once/ });
    // e.g. the second click of a double-click aimed at the previous card.
    fireEvent.click(allow);
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(f.responses).toEqual([]);
    await act(() => new Promise((r) => setTimeout(r, 550)));
    fireEvent.click(allow);
    await waitFor(() => expect(f.responses).toEqual([["r3", "approve"]]));
  });
});
