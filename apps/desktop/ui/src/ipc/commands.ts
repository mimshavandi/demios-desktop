// Typed wrappers over the Rust commands in apps/desktop/src/commands.rs. The UI calls
// only these; `invoke` with a raw string is not used anywhere else.

import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  ApprovalDecision,
  LocalModelEvent,
  LocalModelRow,
  ToolEvent,
  ToolSnapshot,
  UnknownOutcome,
  ChatEvent,
  Conversation,
  Id,
  ModelCatalog,
  Settings,
  StoredMessage,
  TurnStarted,
  LocalDataSummary,
} from "./types";

interface Commands {
  app_info: { args: Record<string, never>; ret: AppInfo };
  delete_all_local_data: { args: Record<string, never>; ret: null };
  local_data_summary: { args: Record<string, never>; ret: LocalDataSummary };
  list_conversations: { args: Record<string, never>; ret: Conversation[] };
  create_conversation: { args: Record<string, never>; ret: Conversation };
  rename_conversation: { args: { conversationId: Id; title: string }; ret: null };
  delete_conversation: { args: { conversationId: Id }; ret: null };
  restore_conversation: { args: { conversationId: Id }; ret: null };
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
  restore_provider_key: { args: { providerId: string }; ret: ModelCatalog };
  watch_tools: { args: { onEvent: Channel<ToolEvent> }; ret: ToolSnapshot };
  respond_approval: { args: { requestId: Id; decision: ApprovalDecision }; ret: null };
  list_unknown_outcomes: { args: { conversationId: Id | null }; ret: UnknownOutcome[] };
  resolve_unknown_outcome: { args: { key: string; happened: boolean }; ret: null };
  watch_local_models: { args: { onEvent: Channel<LocalModelEvent> }; ret: LocalModelRow[] };
  download_model: { args: { modelId: string }; ret: null };
  pause_download: { args: { modelId: string }; ret: null };
}

export type CommandName = keyof Commands;

function call<K extends CommandName>(cmd: K, args: Commands[K]["args"]): Promise<Commands[K]["ret"]> {
  return invoke<Commands[K]["ret"]>(cmd, args);
}

function channel<T>(onEvent: (ev: T) => void): Channel<T> {
  const ch = new Channel<T>();
  ch.onmessage = onEvent;
  return ch;
}

export const api = {
  appInfo: () => call("app_info", {}),
  /** Deletes chats, settings, tool history, models, keychain entries and window storage. */
  deleteAllLocalData: () => call("delete_all_local_data", {}),
  localDataSummary: () => call("local_data_summary", {}),
  listConversations: () => call("list_conversations", {}),
  createConversation: () => call("create_conversation", {}),
  renameConversation: (conversationId: Id, title: string) =>
    call("rename_conversation", { conversationId, title }),
  deleteConversation: (conversationId: Id) => call("delete_conversation", { conversationId }),
  restoreConversation: (conversationId: Id) => call("restore_conversation", { conversationId }),
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
  restoreProviderKey: (providerId: string) => call("restore_provider_key", { providerId }),
  watchTools: (onEvent: (ev: ToolEvent) => void) =>
    call("watch_tools", { onEvent: channel(onEvent) }),
  respondApproval: (requestId: Id, decision: ApprovalDecision) =>
    call("respond_approval", { requestId, decision }),
  listUnknownOutcomes: (conversationId: Id | null) =>
    call("list_unknown_outcomes", { conversationId }),
  resolveUnknownOutcome: (key: string, happened: boolean) =>
    call("resolve_unknown_outcome", { key, happened }),
  watchLocalModels: (onEvent: (ev: LocalModelEvent) => void) =>
    call("watch_local_models", { onEvent: channel(onEvent) }),
  downloadModel: (modelId: string) => call("download_model", { modelId }),
  pauseDownload: (modelId: string) => call("pause_download", { modelId }),
};

export type Api = typeof api;
