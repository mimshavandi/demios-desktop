// IPC contract types. `dn-core` types come from the ts-rs bindings generated from Rust
// (crates/dn-core/bindings, T-CORE-04); never redeclare them here. The dn-store and app
// shapes below mirror serde output by hand and must stay in step with:
//   dn-store:  crates/dn-store/src/chat.rs
//   app:       apps/desktop/src/{chat,commands,models,settings,error}.rs

import type { ApprovalPolicy } from "@dn-core/ApprovalPolicy";
import type { Capability } from "@dn-core/Capability";
import type { ContentPart } from "@dn-core/ContentPart";
import type { FinishReason } from "@dn-core/FinishReason";
import type { Id } from "@dn-core/Id";
import type { RecoveryAction } from "@dn-core/RecoveryAction";
import type { RiskTier } from "@dn-core/RiskTier";
import type { Role } from "@dn-core/Role";
import type { ToolCall } from "@dn-core/ToolCall";
import type { Usage } from "@dn-core/Usage";

export type {
  ApprovalPolicy,
  Capability,
  ContentPart,
  FinishReason,
  Id,
  RecoveryAction,
  RiskTier,
  Role,
  ToolCall,
  Usage,
};

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
  | { type: "text_delta"; message_id: Id; /** UTF-16 length before this chunk. */ offset: number; text: string }
  | { type: "reasoning_delta"; message_id: Id; text: string }
  | {
      type: "tool_call_started";
      message_id: Id;
      call_id: string;
      name: string;
      arguments: unknown;
    }
  | {
      type: "tool_call_finished";
      message_id: Id;
      call_id: string;
      is_error: boolean;
      summary: string;
    }
  | {
      type: "done";
      message_id: Id;
      finish_reason: FinishReason;
      usage: Usage;
      /** Why the run ended; `step_limit` / `length` may be continued. */
      stop?: "completed" | "length" | "step_limit" | "cancelled";
    }
  | { type: "error"; message_id: Id; message: string; recovery: RecoveryAction[] };

export interface TurnStarted {
  user_message: StoredMessage | null;
  assistant_message: StoredMessage;
  title: string | null;
}

// ---- tools (dn-tools via the app)

/** `dn_tools::ApprovalPrompt`. */
export interface ApprovalPrompt {
  request_id: Id;
  /** `conv:<conversation id>` or `run:<run id>`. */
  scope: string;
  capability_id: string;
  title: string;
  description: string;
  risk: RiskTier;
  policy: ApprovalPolicy;
  /** Exactly what the tool will receive; always shown. */
  arguments: unknown;
}

/** Connection state of a configured MCP server. */
export interface McpServerStatus {
  server_id: string;
  name: string;
  state: "connecting" | "connected" | "failed";
  message: string | null;
  tools: number;
}

/** Tool-runtime events (`watch_tools`). */
export type ToolEvent =
  | { type: "requested"; prompt: ApprovalPrompt }
  /** `expired`: timed out or its run ended before an answer. */
  | { type: "resolved"; request_id: Id; expired: boolean }
  | { type: "mcp_server"; status: McpServerStatus };

export interface ToolSnapshot {
  approvals: ApprovalPrompt[];
  servers: McpServerStatus[];
}

export type ApprovalDecision = "approve" | "approve_for_conversation" | "deny";

export interface UnknownOutcome {
  key: string;
  capability_id: string;
  arguments: unknown;
  error: string | null;
  updated_at_ms: number;
}

export interface Settings {
  /** slot id → ModelRef string (`local:…`, `byok:…:…`, `remote:…:…`). */
  slots: Record<string, string>;
  telemetry_enabled: boolean;
  /** Passed through untouched (no screen edits MCP servers yet). */
  mcp_servers?: unknown[];
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
  /** The model chat uses now (chosen or picked automatically); null if none is usable. */
  chat_model: string | null;
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
