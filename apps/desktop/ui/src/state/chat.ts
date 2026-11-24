// Message list state for the open conversation. Pure, so the streaming rules are unit
// tested without a webview. Channel events can arrive before the `send_message` reply
// that announces the assistant message; those are buffered in `pending`.

import type { ChatEvent, Id, StoredMessage, TurnStarted } from "../ipc/types";

export interface ChatState {
  conversationId: Id | null;
  messages: StoredMessage[];
  /** Assistant message currently streaming, if any. */
  streamingId: Id | null;
  pending: Record<Id, ChatEvent[]>;
}

export type ChatAction =
  | { kind: "open"; conversationId: Id | null; messages: StoredMessage[] }
  | { kind: "turn_started"; conversationId: Id; turn: TurnStarted }
  | { kind: "event"; event: ChatEvent }
  | { kind: "turn_failed" };

export const initialChat: ChatState = {
  conversationId: null,
  messages: [],
  streamingId: null,
  pending: {},
};

function applyEvent(m: StoredMessage, ev: ChatEvent): StoredMessage {
  switch (ev.type) {
    case "text_delta": {
      const first = m.content[0];
      const text = (first?.type === "text" ? first.text : "") + ev.text;
      return { ...m, content: [{ type: "text", text }, ...m.content.slice(1)] };
    }
    case "reasoning_delta":
      return m;
    case "done":
      return {
        ...m,
        status: ev.finish_reason === "cancelled" ? "cancelled" : "complete",
        finish_reason: ev.finish_reason,
      };
    case "error":
      return { ...m, status: "error", finish_reason: "error", error: ev.message };
  }
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
        pending: {},
      };
    }
    case "turn_started": {
      if (action.conversationId !== state.conversationId) return state;
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
      for (const ev of buffered) assistant = applyEvent(assistant, ev);
      const { [assistant.id]: _drop, ...pending } = state.pending;
      return {
        ...state,
        messages: [...base, assistant],
        streamingId: buffered.some(isTerminal) ? null : assistant.id,
        pending,
      };
    }
    case "event": {
      const ev = action.event;
      const idx = state.messages.findIndex((m) => m.id === ev.message_id);
      if (idx === -1) {
        const list = state.pending[ev.message_id] ?? [];
        return { ...state, pending: { ...state.pending, [ev.message_id]: [...list, ev] } };
      }
      const messages = state.messages.slice();
      const current = messages[idx];
      if (current === undefined) return state;
      messages[idx] = applyEvent(current, ev);
      const streamingId =
        isTerminal(ev) && state.streamingId === ev.message_id ? null : state.streamingId;
      return { ...state, messages, streamingId };
    }
    case "turn_failed":
      return { ...state, streamingId: null };
  }
}

/** Regenerate is offered on the last message when it is a finished assistant answer. */
export function canRegenerate(state: ChatState): boolean {
  const last = state.messages[state.messages.length - 1];
  return state.streamingId === null && last !== undefined && last.role === "assistant";
}
