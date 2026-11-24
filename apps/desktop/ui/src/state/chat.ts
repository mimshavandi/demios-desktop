// Message list state for the open conversation. Pure, so the streaming rules are unit
// tested without a webview. Channel events can arrive before the `send_message` reply
// that announces the assistant message; while a send is in flight those are buffered in
// `pending`. Events for messages of other (closed) conversations are dropped.

import type { ChatEvent, Id, StoredMessage, TurnStarted } from "../ipc/types";

/** A tool call seen on the stream (before the run's messages are stored). */
export interface LiveToolCall {
  call_id: string;
  name: string;
  arguments: unknown;
  status: "running" | "done" | "error";
  summary: string;
}

export interface ChatState {
  conversationId: Id | null;
  messages: StoredMessage[];
  /** Assistant message currently streaming, if any. */
  streamingId: Id | null;
  /** A send or regenerate is in flight; its events may arrive before the reply. */
  awaiting: boolean;
  pending: Record<Id, ChatEvent[]>;
  /** Live tool calls per assistant message id. */
  tools: Record<Id, LiveToolCall[]>;
}

export type ChatAction =
  | { kind: "open"; conversationId: Id | null; messages: StoredMessage[] }
  | { kind: "sending" }
  | { kind: "turn_started"; conversationId: Id; turn: TurnStarted }
  | { kind: "event"; event: ChatEvent }
  | { kind: "turn_failed" };

export const initialChat: ChatState = {
  conversationId: null,
  messages: [],
  streamingId: null,
  awaiting: false,
  pending: {},
  tools: {},
};

function applyEvent(m: StoredMessage, ev: ChatEvent): StoredMessage {
  switch (ev.type) {
    case "text_delta": {
      const first = m.content[0];
      const have = first?.type === "text" ? first.text : "";
      // Already have this chunk (the message was loaded mid-stream): skip it.
      if (have.length >= ev.offset + ev.text.length) return m;
      const text = have.slice(0, ev.offset) + ev.text;
      return { ...m, content: [{ type: "text", text }, ...m.content.slice(1)] };
    }
    case "reasoning_delta":
    case "tool_call_started":
    case "tool_call_finished":
      return m;
    case "done":
      return {
        ...m,
        status: ev.finish_reason === "cancelled" ? "cancelled" : "complete",
        finish_reason: ev.finish_reason,
      };
    case "error":
      return { ...m, status: "error", finish_reason: "error", error: ev.message };
    case "artifact_changed":
    case "artifact_not_saved":
      return m;
  }
}

function applyTools(tools: Record<Id, LiveToolCall[]>, ev: ChatEvent): Record<Id, LiveToolCall[]> {
  if (ev.type === "tool_call_started") {
    const list = tools[ev.message_id] ?? [];
    const call: LiveToolCall = {
      call_id: ev.call_id,
      name: ev.name,
      arguments: ev.arguments,
      status: "running",
      summary: "",
    };
    return { ...tools, [ev.message_id]: [...list, call] };
  }
  if (ev.type === "tool_call_finished") {
    const list = (tools[ev.message_id] ?? []).map((c) =>
      c.call_id === ev.call_id
        ? { ...c, status: ev.is_error ? ("error" as const) : ("done" as const), summary: ev.summary }
        : c,
    );
    return { ...tools, [ev.message_id]: list };
  }
  return tools;
}

function isTerminal(ev: ChatEvent): boolean {
  return ev.type === "done" || ev.type === "error";
}

export function chatReducer(state: ChatState, action: ChatAction): ChatState {
  switch (action.kind) {
    case "open": {
      const streaming = action.messages.find((m) => m.status === "streaming");
      return {
        conversationId: action.conversationId,
        messages: action.messages,
        streamingId: streaming?.id ?? null,
        awaiting: false,
        pending: {},
        tools: {},
      };
    }
    case "sending":
      return { ...state, awaiting: true };
    case "turn_started": {
      if (action.conversationId !== state.conversationId) return state.awaiting ? { ...state, awaiting: false } : state;
      const { user_message, assistant_message } = action.turn;
      // Regenerate replaces everything after the last user message.
      let base = state.messages;
      if (user_message === null) {
        const lastUser = base.map((m) => m.role).lastIndexOf("user");
        base = base.slice(0, lastUser + 1);
      } else {
        base = [...base, user_message];
      }
      let assistant = assistant_message;
      const buffered = state.pending[assistant.id] ?? [];
      let tools = state.tools;
      for (const ev of buffered) {
        assistant = applyEvent(assistant, ev);
        tools = applyTools(tools, ev);
      }
      const { [assistant.id]: _drop, ...pending } = state.pending;
      return {
        ...state,
        messages: [...base, assistant],
        streamingId: buffered.some(isTerminal) ? null : assistant.id,
        awaiting: false,
        pending,
        tools,
      };
    }
    case "event": {
      const ev = action.event;
      const idx = state.messages.findIndex((m) => m.id === ev.message_id);
      if (idx === -1) {
        if (!state.awaiting) return state;
        const list = state.pending[ev.message_id] ?? [];
        return { ...state, pending: { ...state.pending, [ev.message_id]: [...list, ev] } };
      }
      const messages = state.messages.slice();
      const current = messages[idx];
      if (current === undefined) return state;
      messages[idx] = applyEvent(current, ev);
      const streamingId =
        isTerminal(ev) && state.streamingId === ev.message_id ? null : state.streamingId;
      return { ...state, messages, streamingId, tools: applyTools(state.tools, ev) };
    }
    case "turn_failed":
      return { ...state, streamingId: null, awaiting: false, pending: {} };
  }
}

/** Regenerate is offered on the last message when it is a finished assistant answer. */
export function canRegenerate(state: ChatState): boolean {
  const last = state.messages[state.messages.length - 1];
  return state.streamingId === null && last !== undefined && last.role === "assistant";
}
