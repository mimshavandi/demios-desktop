// IPC contract types. `dn-core` types are mirrored by hand until the TS export
// (ts-rs or specta, lead's call) lands; then the `dn-core` half of this file is replaced
// by generated bindings. Keep every field in step with the Rust serde shapes:
//   dn-core:   crates/dn-core/src/{chat,error,model}.rs
//   dn-store:  crates/dn-store/src/chat.rs
//   app:       apps/desktop/src/{chat,commands,models,settings,error}.rs

/** UUIDv7, serialised as a string (`dn_core::Id`). */
export type Id = string;

// ---- dn-core ----

export type Role = "system" | "user" | "assistant" | "tool";

export type ContentPart =
  | { type: "text"; text: string }
  | { type: "image"; attachment_id: string; mime_type: string };

export interface ToolCall {
  id: string;
  name: string;
  arguments: unknown;
}

export type FinishReason = "stop" | "length" | "tool_calls" | "cancelled" | "error";

export interface Usage {
  input_tokens: number;
  output_tokens: number;
}

export type Capability = "chat" | "vision" | "embedding" | "stt" | "tts" | "image";

export type RecoveryAction =
  | { action: "download_model"; capability: Capability }
  | { action: "add_provider_key" }
  | { action: "choose_model_for_slot"; agent_id: string; slot_id: string }
  | { action: "reconnect_server"; server_id: string }
  | { action: "retry"; after_seconds: number | null };

// ---- dn-store ----

export interface Conversation {
  id: Id;
  title: string;
  model: string | null;
  created_at: number;
  updated_at: number;
}

export type MessageStatus = "streaming" | "complete" | "cancelled" | "error";

export interface StoredMessage {
  id: Id;
  conversation_id: Id;
  role: Role;
  content: ContentPart[];
  tool_calls: ToolCall[];
  tool_call_id: string | null;
  model: string | null;
  status: MessageStatus;
  finish_reason: FinishReason | null;
  error: string | null;
  created_at: number;
  updated_at: number;
}

// ---- app ----

export type ChatEvent =
  | { type: "text_delta"; message_id: Id; text: string }
  | { type: "reasoning_delta"; message_id: Id; text: string }
  | { type: "done"; message_id: Id; finish_reason: FinishReason; usage: Usage }
  | { type: "error"; message_id: Id; message: string; recovery: RecoveryAction[] };

export interface TurnStarted {
  user_message: StoredMessage | null;
  assistant_message: StoredMessage;
  title: string | null;
}

export interface Settings {
  /** slot id → ModelRef string (`local:…`, `byok:…:…`, `remote:…:…`). */
  slots: Record<string, string>;
  telemetry_enabled: boolean;
}

export interface SlotInfo {
  slot_id: string;
  label: string;
  description: string;
  capability: Capability;
}

export type ModelSource = "local" | "byok" | "remote" | "dev";

export interface ModelOption {
  model_ref: string;
  label: string;
  source: ModelSource;
  capabilities: Capability[];
  available: boolean;
  note: string | null;
}

export interface ProviderInfo {
  id: string;
  name: string;
  has_key: boolean;
}

export interface LocalModelPlaceholder {
  model_id: string;
  label: string;
  size: string;
  note: string;
}

export interface ModelCatalog {
  slots: SlotInfo[];
  models: ModelOption[];
  providers: ProviderInfo[];
  local_models: LocalModelPlaceholder[];
  key_store: string;
  settings: Settings;
}

export interface AppInfo {
  version: string;
  dev_fake_model: boolean;
  data_dir: string | null;
}

export interface IpcError {
  code: string;
  message: string;
  recovery?: RecoveryAction[];
}

export function isIpcError(e: unknown): e is IpcError {
  return (
    typeof e === "object" &&
    e !== null &&
    typeof (e as { code?: unknown }).code === "string" &&
    typeof (e as { message?: unknown }).message === "string"
  );
}

export function errorMessage(e: unknown): string {
  if (isIpcError(e)) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export function messageText(m: Pick<StoredMessage, "content">): string {
  return m.content.map((p) => (p.type === "text" ? p.text : "")).join("");
}
