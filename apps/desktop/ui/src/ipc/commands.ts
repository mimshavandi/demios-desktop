// Typed wrappers over the Rust commands in apps/desktop/src/commands.rs. The UI calls
// only these; `invoke` with a raw string is not used anywhere else.

import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  ChatEvent,
  Conversation,
  Id,
  ModelCatalog,
  Settings,
  StoredMessage,
  TurnStarted,
} from "./types";

interface Commands {
  app_info: { args: Record<string, never>; ret: AppInfo };
  list_conversations: { args: Record<string, never>; ret: Conversation[] };
  create_conversation: { args: Record<string, never>; ret: Conversation };
  rename_conversation: { args: { conversationId: Id; title: string }; ret: null };
  delete_conversation: { args: { conversationId: Id }; ret: null };
  list_messages: { args: { conversationId: Id }; ret: StoredMessage[] };
  send_message: {
    args: { conversationId: Id; text: string; onEvent: Channel<ChatEvent> };
    ret: TurnStarted;
  };
  regenerate: { args: { conversationId: Id; onEvent: Channel<ChatEvent> }; ret: TurnStarted };
  stop_generation: { args: { conversationId: Id }; ret: boolean };
  get_settings: { args: Record<string, never>; ret: Settings };
  update_settings: { args: { settings: Settings }; ret: Settings };
  list_models: { args: Record<string, never>; ret: ModelCatalog };
  set_provider_key: { args: { providerId: string; apiKey: string }; ret: ModelCatalog };
  delete_provider_key: { args: { providerId: string }; ret: ModelCatalog };
}

export type CommandName = keyof Commands;

function call<K extends CommandName>(cmd: K, args: Commands[K]["args"]): Promise<Commands[K]["ret"]> {
  return invoke<Commands[K]["ret"]>(cmd, args);
}

function channel(onEvent: (ev: ChatEvent) => void): Channel<ChatEvent> {
  const ch = new Channel<ChatEvent>();
  ch.onmessage = onEvent;
  return ch;
}

export const api = {
  appInfo: () => call("app_info", {}),
  listConversations: () => call("list_conversations", {}),
  createConversation: () => call("create_conversation", {}),
  renameConversation: (conversationId: Id, title: string) =>
    call("rename_conversation", { conversationId, title }),
  deleteConversation: (conversationId: Id) => call("delete_conversation", { conversationId }),
  listMessages: (conversationId: Id) => call("list_messages", { conversationId }),
  sendMessage: (conversationId: Id, text: string, onEvent: (ev: ChatEvent) => void) =>
    call("send_message", { conversationId, text, onEvent: channel(onEvent) }),
  regenerate: (conversationId: Id, onEvent: (ev: ChatEvent) => void) =>
    call("regenerate", { conversationId, onEvent: channel(onEvent) }),
  stopGeneration: (conversationId: Id) => call("stop_generation", { conversationId }),
  getSettings: () => call("get_settings", {}),
  updateSettings: (settings: Settings) => call("update_settings", { settings }),
  listModels: () => call("list_models", {}),
  setProviderKey: (providerId: string, apiKey: string) =>
    call("set_provider_key", { providerId, apiKey }),
  deleteProviderKey: (providerId: string) => call("delete_provider_key", { providerId }),
};

export type Api = typeof api;
