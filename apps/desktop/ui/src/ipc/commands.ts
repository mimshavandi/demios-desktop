// Typed wrappers over the Rust commands in apps/desktop/src/commands.rs. The UI calls
// only these; `invoke` with a raw string is not used anywhere else.

import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  KnowledgeStatus,
  MessageSources,
  AppInfo,
  ApprovalDecision,
  ArtifactDetail,
  ArtifactSummary,
  LocalModelEvent,
  LocalModelRow,
  ToolEvent,
  ToolSnapshot,
  UnknownOutcome,
  ChatEvent,
  Conversation,
  SkillInfo,
  SkillsOverview,
  Id,
  AddedFiles,
  Project,
  ProjectFile,
  ProjectSummary,
  ModelCatalog,
  Settings,
  StoredMessage,
  TurnStarted,
  LocalDataSummary,
  ReportKind,
  McpServerInput,
  McpServersView,
  McpServerToolsView,
  ToolChoice,
  HubPullEvent,
  HubRepoView,
  HubSearchPage,
  HubSort,
  HubPullProgress,
  AddModelLink,
  UpdateCheck,
  UpdateProgress,
} from "./types";

interface Commands {
  app_info: { args: Record<string, never>; ret: AppInfo };
  check_for_update: { args: Record<string, never>; ret: UpdateCheck };
  install_update: { args: { version: string; onProgress: Channel<UpdateProgress> }; ret: null };
  delete_all_local_data: { args: Record<string, never>; ret: null };
  local_data_summary: { args: Record<string, never>; ret: LocalDataSummary };
  report_problem: { args: { kind: ReportKind; messageId: Id | null }; ret: null };
  open_link: { args: { url: string }; ret: boolean };
  list_mcp_servers: { args: Record<string, never>; ret: McpServersView };
  save_mcp_server: { args: { input: McpServerInput; editing: string | null }; ret: McpServersView };
  remove_mcp_server: { args: { serverId: string }; ret: McpServersView };
  reconnect_mcp_server: { args: { serverId: string }; ret: null };
  sign_in_mcp_server: { args: { serverId: string }; ret: null };
  sign_out_mcp_server: { args: { serverId: string }; ret: McpServersView };
  list_mcp_tools: { args: { serverId: string }; ret: McpServerToolsView };
  set_mcp_tool_choice: { args: { serverId: string; tool: string; choice: ToolChoice }; ret: McpServerToolsView };
  mark_mcp_tools_reviewed: {
    args: { serverId: string; tools: { name: string; fingerprint: string }[] };
    ret: McpServerToolsView;
  };
  hub_search: { args: { text: string; sort: HubSort; cursor: string | null }; ret: HubSearchPage };
  hub_repo: { args: { repo: string }; ret: HubRepoView };
  hub_pull: { args: { repo: string; sha: string; variant: string; onEvent: Channel<HubPullEvent> }; ret: string };
  hub_cancel: { args: { repo: string; sha: string; variant: string }; ret: boolean };
  hub_pull_status: { args: { repo: string; sha: string; variants: string[] }; ret: Record<string, HubPullProgress> };
  watch_add_links: { args: { onEvent: Channel<AddModelLink> }; ret: AddModelLink | null };
  add_link_done: { args: Record<string, never>; ret: null };
  hub_resolve_link: { args: { link: AddModelLink }; ret: HubRepoView };
  hf_token_status: { args: Record<string, never>; ret: boolean };
  set_hf_token: { args: { token: string }; ret: boolean };
  delete_hf_token: { args: Record<string, never>; ret: boolean };
  list_conversations: { args: Record<string, never>; ret: Conversation[] };
  create_conversation: { args: { projectId: Id | null }; ret: Conversation };
  list_projects: { args: Record<string, never>; ret: ProjectSummary[] };
  create_project: { args: { name: string }; ret: Project };
  update_project: { args: { projectId: Id; name: string; instructions: string }; ret: Project };
  delete_project: { args: { projectId: Id }; ret: number };
  restore_project: { args: { projectId: Id }; ret: null };
  move_conversation: { args: { conversationId: Id; projectId: Id | null }; ret: Id | null };
  list_project_files: { args: { projectId: Id }; ret: ProjectFile[] };
  add_project_files: { args: { projectId: Id }; ret: AddedFiles };
  remove_project_file: { args: { projectId: Id; fileId: Id }; ret: null };
  restore_project_file: { args: { projectId: Id; fileId: Id }; ret: null };
  rename_conversation: { args: { conversationId: Id; title: string }; ret: null };
  delete_conversation: { args: { conversationId: Id }; ret: null };
  restore_conversation: { args: { conversationId: Id }; ret: null };
  list_skills: { args: Record<string, never>; ret: SkillInfo[] };
  skills_overview: { args: Record<string, never>; ret: SkillsOverview };
  reload_skills: { args: Record<string, never>; ret: SkillsOverview };
  open_skills_folder: { args: Record<string, never>; ret: null };
  get_chat_skill: { args: { conversationId: Id }; ret: string | null };
  set_chat_skill: { args: { conversationId: Id; skill: string | null }; ret: null };
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
  list_artifacts: { args: { conversationId: Id }; ret: ArtifactSummary[] };
  get_artifact: { args: { conversationId: Id; artifactId: Id }; ret: ArtifactDetail };
  rollback_artifact: {
    args: { conversationId: Id; artifactId: Id; toVersion: number };
    ret: ArtifactDetail;
  };
  watch_local_models: { args: { onEvent: Channel<LocalModelEvent> }; ret: LocalModelRow[] };
  download_model: { args: { modelId: string }; ret: null };
  pause_download: { args: { modelId: string }; ret: null };
  // Windows artifact webview (artifactWebview.ts, ADR-0012).
  artifact_child_open: { args: { token: string; background: string | null; events: Channel<ChildEvent> }; ret: null };
  artifact_child_render: { args: { seq: number; html: string; nonce: string; net: boolean; title: string }; ret: null };
  artifact_child_bounds: { args: { bounds: ChildBounds | null }; ret: null };
  artifact_child_focus: { args: Record<string, never>; ret: null };
  artifact_child_close: { args: Record<string, never>; ret: null };
  // Knowledge search (knowledge.rs, #116).
  message_sources: { args: { messageId: Id }; ret: MessageSources | null };
  knowledge_status: { args: Record<string, never>; ret: KnowledgeStatus };
  knowledge_offer: { args: { accept: boolean }; ret: null };
}

/** Where the artifact webview goes, in this webview's CSS pixels. */
export interface ChildBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** From the artifact webview, through Rust (`artifact_child.rs` ChildEvent). */
export type ChildEvent =
  | { kind: "frame"; seq: number; data: unknown }
  | { kind: "refused"; seq: number }
  | { kind: "focus-out"; dir: "prev" | "next" };

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
  artifactChildOpen: (token: string, background: string | null, events: Channel<ChildEvent>) =>
    call("artifact_child_open", { token, background, events }),
  artifactChildRender: (doc: Commands["artifact_child_render"]["args"]) => call("artifact_child_render", doc),
  artifactChildBounds: (bounds: ChildBounds | null) => call("artifact_child_bounds", { bounds }),
  artifactChildFocus: () => call("artifact_child_focus", {}),
  artifactChildClose: () => call("artifact_child_close", {}),
  /** What an answer in a project chat was given from the project's files (null: not a project chat). */
  messageSources: (messageId: Id) => call("message_sources", { messageId }),
  /** The embedding-model offer and the progress of improving search (#171). */
  knowledgeStatus: () => call("knowledge_status", {}),
  /** "Download" (true) or "Not now" (false); remembered. */
  knowledgeOffer: (accept: boolean) => call("knowledge_offer", { accept }),
  appInfo: () => call("app_info", {}),
  /** Asks GitHub Releases for a newer version; downloads nothing. */
  checkForUpdate: () => call("check_for_update", {}),
  /** After the user confirmed `version`: downloads, verifies, installs and restarts. */
  installUpdate: (version: string, onProgress: (p: UpdateProgress) => void) =>
    call("install_update", { version, onProgress: channel(onProgress) }),
  /** Deletes chats, settings, tool history, models, keychain entries and window storage. */
  deleteAllLocalData: () => call("delete_all_local_data", {}),
  localDataSummary: () => call("local_data_summary", {}),
  listMcpServers: () => call("list_mcp_servers", {}),
  /** Adds (`editing: null`) or updates a server, then connects it. */
  saveMcpServer: (input: McpServerInput, editing: string | null) => call("save_mcp_server", { input, editing }),
  /** Deletes the server with its secrets and sign-in. Can't be undone. */
  removeMcpServer: (serverId: string) => call("remove_mcp_server", { serverId }),
  reconnectMcpServer: (serverId: string) => call("reconnect_mcp_server", { serverId }),
  /** Opens the server's sign-in page in the browser and waits (up to 5 minutes). */
  signInMcpServer: (serverId: string) => call("sign_in_mcp_server", { serverId }),
  /** Deletes the server's stored sign-in tokens. */
  signOutMcpServer: (serverId: string) => call("sign_out_mcp_server", { serverId }),
  /** A server's tools with their risk tier, approval and review flag. */
  listMcpTools: (serverId: string) => call("list_mcp_tools", { serverId }),
  /** Saves the user's choice for one tool and applies it now. */
  setMcpToolChoice: (serverId: string, tool: string, choice: ToolChoice) =>
    call("set_mcp_tool_choice", { serverId, tool, choice }),
  /** Marks new or changed tools reviewed (only the versions the user was shown). */
  markMcpToolsReviewed: (serverId: string, tools: { name: string; fingerprint: string }[]) =>
    call("mark_mcp_tools_reviewed", { serverId, tools }),
  hubSearch: (text: string, sort: HubSort, cursor: string | null = null) => call("hub_search", { text, sort, cursor }),
  hubRepo: (repo: string) => call("hub_repo", { repo }),
  /** Downloads one quantisation (verified, resumable); resolves to its model ref. */
  hubPull: (repo: string, sha: string, variant: string, onEvent: (ev: HubPullEvent) => void) =>
    call("hub_pull", { repo, sha, variant, onEvent: channel(onEvent) }),
  /** Pauses a running download; downloading again resumes it. */
  hubCancel: (repo: string, sha: string, variant: string) => call("hub_cancel", { repo, sha, variant }),
  /** Progress of running downloads of these variants (for a screen that came back). */
  hubPullStatus: (repo: string, sha: string, variants: string[]) => call("hub_pull_status", { repo, sha, variants }),
  /** demios://models/add links from the OS; resolves to one that arrived before subscribing. */
  watchAddLinks: (onEvent: (l: AddModelLink) => void) => call("watch_add_links", { onEvent: channel(onEvent) }),
  /** The link's confirmation closed: the next link may open it. */
  addLinkDone: () => call("add_link_done", {}),
  /** What a link asks for, for the confirmation screen. Downloads nothing. */
  hubResolveLink: (link: AddModelLink) => call("hub_resolve_link", { link }),
  hfTokenStatus: () => call("hf_token_status", {}),
  setHfToken: (token: string) => call("set_hf_token", { token }),
  deleteHfToken: () => call("delete_hf_token", {}),
  /**
   * Opens the prefilled issue form in the browser. For an answer, pass its message id: the
   * backend reads the model from the database. No chat content is sent.
   */
  reportProblem: (kind: ReportKind, messageId: Id | null = null) => call("report_problem", { kind, messageId }),
  /** Asks to open a link from an answer; the backend confirms with the full URL first. */
  openLink: (url: string) => call("open_link", { url }),
  listConversations: () => call("list_conversations", {}),
  /** A new chat, in `projectId` when given. */
  createConversation: (projectId: Id | null = null) => call("create_conversation", { projectId }),
  listProjects: () => call("list_projects", {}),
  createProject: (name: string) => call("create_project", { name }),
  updateProject: (projectId: Id, name: string, instructions: string) =>
    call("update_project", { projectId, name, instructions }),
  /** Soft delete with its chats and files; returns how many chats went with it. */
  deleteProject: (projectId: Id) => call("delete_project", { projectId }),
  restoreProject: (projectId: Id) => call("restore_project", { projectId }),
  /** Moves a chat into a project, or out with `null`; returns the project it was in, for Undo. */
  moveConversation: (conversationId: Id, projectId: Id | null) =>
    call("move_conversation", { conversationId, projectId }),
  listProjectFiles: (projectId: Id) => call("list_project_files", { projectId }),
  /** Opens the system file picker (shown by the backend) and copies the picked files in. */
  addProjectFiles: (projectId: Id) => call("add_project_files", { projectId }),
  removeProjectFile: (projectId: Id, fileId: Id) => call("remove_project_file", { projectId, fileId }),
  restoreProjectFile: (projectId: Id, fileId: Id) => call("restore_project_file", { projectId, fileId }),
  renameConversation: (conversationId: Id, title: string) =>
    call("rename_conversation", { conversationId, title }),
  deleteConversation: (conversationId: Id) => call("delete_conversation", { conversationId }),
  restoreConversation: (conversationId: Id) => call("restore_conversation", { conversationId }),
  listMessages: (conversationId: Id) => call("list_messages", { conversationId }),
  listSkills: () => call("list_skills", {}),
  skillsOverview: () => call("skills_overview", {}),
  /** Reads the skills folders again. */
  reloadSkills: () => call("reload_skills", {}),
  /** Opens the user's skills folder (created if missing) in the file manager. */
  openSkillsFolder: () => call("open_skills_folder", {}),
  getChatSkill: (conversationId: Id) => call("get_chat_skill", { conversationId }),
  /** Applies from the chat's next answer on; `null` = no skill. */
  setChatSkill: (conversationId: Id, skill: string | null) =>
    call("set_chat_skill", { conversationId, skill }),
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
  listArtifacts: (conversationId: Id) => call("list_artifacts", { conversationId }),
  getArtifact: (conversationId: Id, artifactId: Id) =>
    call("get_artifact", { conversationId, artifactId }),
  rollbackArtifact: (conversationId: Id, artifactId: Id, toVersion: number) =>
    call("rollback_artifact", { conversationId, artifactId, toVersion }),
  watchLocalModels: (onEvent: (ev: LocalModelEvent) => void) =>
    call("watch_local_models", { onEvent: channel(onEvent) }),
  downloadModel: (modelId: string) => call("download_model", { modelId }),
  pauseDownload: (modelId: string) => call("pause_download", { modelId }),
};

export type Api = typeof api;
