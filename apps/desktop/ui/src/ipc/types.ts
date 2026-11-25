// IPC contract types. `dn-core` types come from the ts-rs bindings generated from Rust
// (crates/dn-core/bindings, T-CORE-04); never redeclare them here. The dn-store and app
// shapes below mirror serde output by hand and must stay in step with:
//   dn-store:  crates/dn-store/src/chat.rs
//   app:       apps/desktop/src/{chat,commands,models,settings,error}.rs

import type { ApprovalPolicy } from "@dn-core/ApprovalPolicy";
import type { ArtifactType } from "@dn-core/ArtifactType";
import type { ArtifactVersion } from "@dn-core/ArtifactVersion";
import type { Capability } from "@dn-core/Capability";
import type { FitStatus } from "@dn-core/FitStatus";
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
  ArtifactType,
  ArtifactVersion,
  Capability,
  ContentPart,
  FitStatus,
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
  /** The project this chat belongs to, if any. */
  project_id: Id | null;
  created_at: number;
  updated_at: number;
}

/** A project: chats, files and standing instructions grouped together (T-PRJ-01). */
export interface Project {
  id: Id;
  name: string;
  /** Sent with every chat of the project; "" for none. */
  instructions: string;
  created_at: number;
  updated_at: number;
}

export interface ProjectSummary extends Project {
  /** Live chats in the project. */
  chats: number;
  files: number;
}

/** A file added to a project: a copy kept in the app data folder. */
export interface ProjectFile {
  id: Id;
  project_id: Id;
  /** Display name, plain text. */
  name: string;
  media_type: string;
  size: number;
  sha256: string;
  /** Where it was added from (shown only). */
  source_path: string | null;
  created_at: number;
  updated_at: number;
}

export interface AddedFiles {
  /** The file picker was closed without a choice. */
  cancelled: boolean;
  added: ProjectFile[];
  skipped: { name: string; reason: string }[];
}

/** Limits the backend enforces (apps/desktop/src/projects.rs, dn_store::project). */
export const PROJECT_NAME_MAX = 100;
export const PROJECT_INSTRUCTIONS_MAX = 8000;

/** A chat skill from the library (apps/desktop/src/skills.rs `SkillInfo`). */
export interface SkillInfo {
  name: string;
  label: string;
  description: string;
  /** Tool patterns it allows; `[]` means no tools. It never changes approvals. */
  allowed_tools: string[];
  /** Shipped with the app, or a file in the user's skills folder. */
  source: "builtin" | "user";
}

/** A file in the user's skills folder that didn't load (skills.rs `SkillProblem`). */
export interface SkillProblem {
  /** Relative to the skills folder, e.g. `my-skill/SKILL.md`. */
  file: string;
  message: string;
}

/** Settings › Skills (skills.rs `SkillsOverview`). */
export interface SkillsOverview {
  skills: SkillInfo[];
  problems: SkillProblem[];
  folder: string | null;
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
  | { type: "error"; message_id: Id; message: string; recovery: RecoveryAction[] }
  /** An artifact was created or got a new version in this turn. */
  | {
      type: "artifact_changed";
      message_id: Id;
      conversation_id: Id;
      artifact_id: Id;
      title: string;
      version: number;
    };

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
  state: "connecting" | "connected" | "needs_auth" | "signing_in" | "failed";
  message: string | null;
  tools: number;
  /** Tools that are new or changed since they were reviewed; they always ask meanwhile. */
  review: number;
  /** Bumped when the server changed its tools mid-session, so the Tools view reloads. */
  revision: number;
}

/** Tool-runtime events (`watch_tools`). */
export type ToolEvent =
  | { type: "requested"; prompt: ApprovalPrompt }
  /** `expired`: timed out or its run ended before an answer. */
  | { type: "resolved"; request_id: Id; expired: boolean }
  | { type: "mcp_server"; status: McpServerStatus }
  | { type: "mcp_server_removed"; server_id: string };

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
  /** First-run AI notice dismissed (T-STORE-09). Missing in older saves = not seen. */
  ai_notice_seen?: boolean;
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

/** Where a catalogue model is (design-system §4, Download row). */
export type DownloadState =
  | { state: "not_downloaded" }
  /** `resuming`: a partial file is re-checked before the transfer continues. */
  | { state: "queued"; resuming: boolean }
  | { state: "downloading"; done: number; total: number; bytes_per_sec: number }
  | { state: "paused"; done: number; total: number }
  | { state: "verifying"; total: number }
  | { state: "ready" }
  | { state: "failed"; message: string; done: number; total: number };

export interface FitView {
  status: FitStatus;
  /** From the file size only (before download); exact after. */
  estimated: boolean;
  reason: string;
  suggested_quant: string | null;
}

/** A curated catalogue model on (or for) this device. */
/** Where a local model comes from (T-INF-10). */
export type ModelOrigin = "catalog" | "hub" | "hf_cache";

export interface LocalModelRow {
  id: string;
  origin: ModelOrigin;
  /** Hub and cache models: where the file is. */
  location: string | null;
  model_ref: string;
  name: string;
  repo: string;
  quant: string;
  size: number;
  params_b: number;
  capabilities: Capability[];
  license: string;
  /** Missing, non-commercial or custom licence. */
  license_warning: boolean;
  card_url: string;
  notes: string;
  state: DownloadState;
  fit: FitView;
}

export type LocalModelEvent = { type: "changed"; row: LocalModelRow } | { type: "removed"; id: string };

// ---- add a model from Hugging Face (T-INF-10)

export type HubSort = "downloads" | "likes" | "trending" | "updated";

export interface HubRepoSummary {
  id: string;
  author: string;
  downloads: number;
  likes: number;
  last_modified: string | null;
  gated: boolean;
  pipeline_tag: string | null;
  license: string | null;
  tags: string[];
}

export interface HubSearchPage {
  items: HubRepoSummary[];
  next: string | null;
}

export type LicenseWarning = "missing" | "non_commercial" | "custom" | "use_policy";

export interface HubLicense {
  id: string | null;
  name: string | null;
  link: string | null;
  warning: LicenseWarning | null;
}

export interface HubVariant {
  name: string;
  quant: string | null;
  total_size: number;
  parts: number;
  /** Every part has a published checksum; only then can it be downloaded. */
  verifiable: boolean;
  fit: FitView | null;
  /** Already downloaded: its model ref. */
  downloaded: string | null;
  /** Downloading now (started earlier): the latest progress. */
  progress: HubPullProgress | null;
}

export interface HubRepoView {
  id: string;
  sha: string;
  gated: boolean;
  license: HubLicense;
  card_url: string;
  variants: HubVariant[];
  /** The variant a demios:// link chose, if it chose exactly one. */
  selected: string | null;
  /** The fit couldn't be worked out (header unreadable). */
  fit_unknown: boolean;
}

/** A demios://models/add link, validated by the backend (T-INF-12). */
export interface AddModelLink {
  repo: string;
  file: string | null;
  quant: string | null;
  revision: string | null;
}

export interface HubPullProgress {
  part: number;
  parts: number;
  done_bytes: number;
  total_bytes: number;
}

export type HubPullEvent = { type: "progress" } & HubPullProgress;

export interface ModelCatalog {
  slots: SlotInfo[];
  models: ModelOption[];
  providers: ProviderInfo[];
  local_models: LocalModelRow[];
  key_store: string;
  settings: Settings;
  /** The model chat uses now (chosen or picked automatically); null if none is usable. */
  chat_model: string | null;
  /** Whether the chat model makes tool calls; without them it can't make artifacts. */
  chat_model_tools: boolean;
}

export interface AppInfo {
  version: string;
  dev_fake_model: boolean;
  data_dir: string | null;
  /** Where downloaded models live (shared with the `dn` CLI). */
  models_dir: string | null;
  /** How this build gets updates (T-STORE-08). */
  updates: UpdateMode;
  /** Commit the build was made from (CI builds only). */
  commit?: string | null;
  /** Build channel, e.g. "preview" (CI builds only). */
  channel?: string | null;
}

/**
 * How a build gets updates: `self_update` checks GitHub Releases when asked; Store and
 * Flathub builds are updated by their store; `no_key` and `off` builds don't update.
 */
export type UpdateMode = "self_update" | "no_key" | "microsoft_store" | "flathub" | "off";

/** What "Check for updates" found. Nothing is downloaded by a check. */
export type UpdateCheck =
  | { status: "up_to_date"; current: string }
  | { status: "available"; version: string; current: string; notes: string; date: string | null };

/** Progress of "Install and restart". */
export type UpdateProgress = { kind: "downloading"; downloaded: number; total: number | null } | { kind: "installing" };

// ---- MCP servers screen (T-SHELL-08). Secret values never come back from the backend.

/** An env var or header. `value` is set for plain values only; `stored` says a secret is saved. */
export interface McpValueView {
  name: string;
  secret: boolean;
  value: string | null;
  stored: boolean;
}

export type McpTransportView =
  | { type: "stdio"; command: string; args: string[]; env: McpValueView[] }
  | {
      type: "streamable_http";
      url: string;
      headers: McpValueView[];
      oauth: { scopes: string[]; client_id: string | null; signed_in: boolean } | null;
    };

export interface McpServerView {
  id: string;
  name: string;
  transport: McpTransportView;
  status: McpServerStatus | null;
}

export interface McpServersView {
  servers: McpServerView[];
  /** False in builds that can't start local programs (Flatpak): HTTP servers only. */
  local_servers_allowed: boolean;
}

/** A value from the form. For a secret, `value: null` keeps the stored one. */
export interface McpValueInput {
  name: string;
  secret: boolean;
  value: string | null;
}

export type McpTransportInput =
  | { type: "stdio"; command: string; args: string[]; env: McpValueInput[] }
  | {
      type: "streamable_http";
      url: string;
      headers: McpValueInput[];
      oauth: boolean;
      scopes: string[];
      client_id: string | null;
    };

export interface McpServerInput {
  id: string;
  name: string | null;
  transport: McpTransportInput;
}

/** "Report a problem" (app) or "Report this answer" (a model answer). */
export type ReportKind = "problem" | "answer";

/** What "Delete all local data" removes (confirm dialog). */
export interface LocalDataSummary {
  conversations: number;
  projects: number;
  /** Artifacts in the database. */
  artifacts: number;
  data_dir: string | null;
  models_dir: string | null;
  models_bytes: number;
  /** Keychain entries in the user's words ("OpenAI key", "Hugging Face token", …). */
  keychain: string[];
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

/** The IPC error code, if `e` is one. */
export function errorCode(e: unknown): string | null {
  return isIpcError(e) ? e.code : null;
}

export function errorMessage(e: unknown): string {
  if (isIpcError(e)) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export function messageText(m: Pick<StoredMessage, "content">): string {
  return m.content.map((p) => (p.type === "text" ? p.text : "")).join("");
}

// ---- each server's tools (T-SHELL-11)

/** The user's choice for one tool. `default` follows its risk tier. */
export type ToolChoice = "default" | "always_ask" | "ask_once_per_chat" | "run_without_asking" | "off";

export interface McpToolView {
  /** Capability id, `mcp.<server>.<tool>`. */
  id: string;
  name: string;
  title: string | null;
  /** From the server: plain text, never rendered as Markdown or HTML. */
  description: string;
  seeded_risk: RiskTier;
  /** In force now, after the user's choice and any review flag. */
  approval: ApprovalPolicy;
  runs_without_asking: boolean;
  disabled: boolean;
  choice: ToolChoice;
  flag: "new" | "changed" | null;
  /** Sent back with "Mark reviewed". */
  fingerprint: string;
}

export interface McpServerToolsView {
  server_id: string;
  connected: boolean;
  tools: McpToolView[];
}

// ---- artifacts (apps/desktop/src/artifacts.rs, dn-artifacts)

/** `dn_artifacts::ArtifactSummary`. */
export interface ArtifactSummary {
  id: Id;
  conversation_id: Id;
  artifact_type: ArtifactType;
  title: string;
  latest_n: number;
  created_at: string;
  updated_at: string;
}

/** A version plus the model that made it (`null` for the user's own restores). */
export type ArtifactVersionView = ArtifactVersion & { model: string | null };

/** `artifacts::ArtifactView`: one artifact with every version. */
export interface ArtifactDetail {
  id: Id;
  conversation_id: Id;
  artifact_type: ArtifactType;
  title: string;
  versions: ArtifactVersionView[];
}
