// IPC contract types. `dn-core` types come from the ts-rs bindings generated from Rust
// (crates/dn-core/bindings, T-CORE-04); never redeclare them here. The dn-store and app
// shapes below mirror serde output by hand and must stay in step with:
//   dn-store:  crates/dn-store/src/chat.rs
//   app:       apps/desktop/src/{chat,commands,models,settings,error}.rs

import type { Capability } from "@dn-core/Capability";
import type { ContentPart } from "@dn-core/ContentPart";
import type { FinishReason } from "@dn-core/FinishReason";
import type { Id } from "@dn-core/Id";
import type { RecoveryAction } from "@dn-core/RecoveryAction";
import type { Role } from "@dn-core/Role";
import type { ToolCall } from "@dn-core/ToolCall";
import type { Usage } from "@dn-core/Usage";

export type { Capability, ContentPart, FinishReason, Id, RecoveryAction, Role, ToolCall, Usage };

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
