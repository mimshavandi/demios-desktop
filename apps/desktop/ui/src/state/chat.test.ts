import { beforeEach, describe, expect, it } from "vitest";
import type { ChatEvent, StoredMessage } from "../ipc/types";
import { canRegenerate, chatReducer, initialChat, type ChatState } from "./chat";

function msg(id: string, role: StoredMessage["role"], text: string, status: StoredMessage["status"] = "complete"): StoredMessage {
  return {
    id,
    conversation_id: "c1",
    role,
    content: text === "" ? [] : [{ type: "text", text }],
    tool_calls: [],
    tool_call_id: null,
    model: null,
    status,
    finish_reason: null,
    error: null,
    created_at: 0,
    updated_at: 0,
  };
}

const opened: ChatState = chatReducer(initialChat, { kind: "open", conversationId: "c1", messages: [] });
let off = 0;
const delta = (text: string, id = "a1", offset?: number): ChatEvent => {
  const ev: ChatEvent = { type: "text_delta", message_id: id, offset: offset ?? off, text };
  off = (offset ?? off) + text.length;
  return ev;
};
const done = (id = "a1"): ChatEvent => ({
  type: "done",
  message_id: id,
  finish_reason: "stop",
  usage: { input_tokens: 1, output_tokens: 2 },
});

describe("chatReducer", () => {
  beforeEach(() => {
    off = 0;
  });

  it("appends deltas to the streaming message and ends on done", () => {
    let s = chatReducer(opened, {
      kind: "turn_started",
      conversationId: "c1",
      turn: { user_message: msg("u1", "user", "hi"), assistant_message: msg("a1", "assistant", "", "streaming"), title: "hi" },
    });
    expect(s.streamingId).toBe("a1");
    expect(canRegenerate(s)).toBe(false);
    s = chatReducer(s, { kind: "event", event: delta("Hel") });
    s = chatReducer(s, { kind: "event", event: delta("lo") });
    expect(s.messages[1]?.content).toEqual([{ type: "text", text: "Hello" }]);
    s = chatReducer(s, { kind: "event", event: done() });
    expect(s.streamingId).toBeNull();
    expect(s.messages[1]?.status).toBe("complete");
    expect(canRegenerate(s)).toBe(true);
  });

  it("buffers events that arrive before the turn is announced", () => {
    let s = chatReducer(opened, { kind: "sending" });
    s = chatReducer(s, { kind: "event", event: delta("early ") });
    s = chatReducer(s, { kind: "event", event: delta("bird") });
    expect(s.pending["a1"]).toHaveLength(2);
    s = chatReducer(s, {
      kind: "turn_started",
      conversationId: "c1",
      turn: { user_message: msg("u1", "user", "x"), assistant_message: msg("a1", "assistant", "", "streaming"), title: null },
    });
    expect(s.pending).toEqual({});
    expect(s.messages[1]?.content).toEqual([{ type: "text", text: "early bird" }]);
    expect(s.streamingId).toBe("a1");
  });

  it("a finished buffered stream is not left streaming", () => {
    let s = chatReducer(opened, { kind: "sending" });
    s = chatReducer(s, { kind: "event", event: delta("all") });
    s = chatReducer(s, { kind: "event", event: done() });
    s = chatReducer(s, {
      kind: "turn_started",
      conversationId: "c1",
      turn: { user_message: msg("u1", "user", "x"), assistant_message: msg("a1", "assistant", "", "streaming"), title: null },
    });
    expect(s.streamingId).toBeNull();
    expect(s.messages[1]?.status).toBe("complete");
  });

  it("regenerate replaces the answer after the last user message", () => {
    let s = chatReducer(initialChat, {
      kind: "open",
      conversationId: "c1",
      messages: [msg("u1", "user", "q"), msg("a1", "assistant", "old")],
    });
    s = chatReducer(s, {
      kind: "turn_started",
      conversationId: "c1",
      turn: { user_message: null, assistant_message: msg("a2", "assistant", "", "streaming"), title: null },
    });
    expect(s.messages.map((m) => m.id)).toEqual(["u1", "a2"]);
  });

  it("marks stopped and failed answers", () => {
    let s = chatReducer(initialChat, {
      kind: "open",
      conversationId: "c1",
      messages: [msg("u1", "user", "q"), msg("a1", "assistant", "part", "streaming"), msg("a2", "assistant", "", "streaming")],
    });
    expect(s.streamingId).toBe("a1");
    s = chatReducer(s, {
      kind: "event",
      event: { type: "done", message_id: "a1", finish_reason: "cancelled", usage: { input_tokens: 0, output_tokens: 0 } },
    });
    expect(s.messages[1]?.status).toBe("cancelled");
    s = chatReducer(s, { kind: "event", event: { type: "error", message_id: "a2", message: "boom", recovery: [] } });
    expect(s.messages[2]?.status).toBe("error");
    expect(s.messages[2]?.error).toBe("boom");
  });

  it("ignores a turn for another conversation", () => {
    const s = chatReducer(opened, {
      kind: "turn_started",
      conversationId: "other",
      turn: { user_message: msg("u1", "user", "x"), assistant_message: msg("a1", "assistant", ""), title: null },
    });
    expect(s).toBe(opened);
  });

  it("drops events for messages of closed conversations", () => {
    const s = chatReducer(opened, { kind: "event", event: delta("stray", "other") });
    expect(s).toBe(opened);
  });

  it("skips chunks it already has after reopening mid-stream", () => {
    let s = chatReducer(initialChat, {
      kind: "open",
      conversationId: "c1",
      messages: [msg("u1", "user", "q"), msg("a1", "assistant", "Hello wor", "streaming")],
    });
    s = chatReducer(s, { kind: "event", event: delta("Hello ", "a1", 0) });
    s = chatReducer(s, { kind: "event", event: delta("wor", "a1", 6) });
    s = chatReducer(s, { kind: "event", event: delta("ld", "a1", 9) });
    expect(s.messages[1]?.content).toEqual([{ type: "text", text: "Hello world" }]);
  });
});
