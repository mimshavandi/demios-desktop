import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Api } from "../ipc/commands";
import type {
  AddModelLink,
  ApprovalPrompt,
  ArtifactDetail,
  ChatEvent,
  Conversation,
  ModelCatalog,
  Project,
  ProjectFile,
  Settings,
  StoredMessage,
  LocalModelEvent,
  LocalModelRow,
  ToolEvent,
  UnknownOutcome,
  UpdateCheck,
  UpdateMode,
} from "../ipc/types";
import { App } from "./App";
import { KEYCHAIN_SLOW_MS, KeychainNotice } from "./SettingsView";
import { ToastProvider } from "./toast";

afterEach(() => {
  cleanup();
  localStorage.clear();
});

// The real renderer needs a webview (constructable stylesheets, sandboxed frames); record
// what the pane hands it instead. Its own behaviour is tested in ui/artifact-sandbox.
const shown = vi.hoisted(() => ({
  artifacts: [] as { id: string; versions: unknown[] }[],
  madeBy: [] as (string | null)[],
  opts: null as null | { allowNetwork?: (id: string) => boolean; onAllowNetwork?: (id: string) => void },
}));
vi.mock("@dn-artifacts", () => ({
  ArtifactView: class {
    constructor(
      private el: HTMLElement,
      private opts: { madeBy?: (v: { author: string; model?: string | null }) => string | null },
    ) {
      this.el.append(Object.assign(document.createElement("div"), { textContent: "sandbox view" }));
      shown.opts = this.opts as typeof shown.opts;
    }
    setArtifact(a: { id: string; versions: { author: string; model?: string | null }[] }) {
      shown.artifacts.push(a);
      const last = a.versions[a.versions.length - 1];
      shown.madeBy.push(last ? (this.opts.madeBy?.(last) ?? null) : null);
      return Promise.resolve();
    }
    destroy() {}
  },
}));

// Radix Select (the skill picker) calls these on open; jsdom lacks them.
Element.prototype.hasPointerCapture ??= () => false;
Element.prototype.releasePointerCapture ??= () => {};
Element.prototype.scrollIntoView ??= () => {};

/** Opens Settings on a section through the section list (#174). */
async function openSettings(section: "General" | "Models" | "Tools" | "Skills") {
  fireEvent.click(await screen.findByRole("button", { name: /^Settings/ }));
  const nav = await screen.findByRole("navigation", { name: "Settings sections" });
  fireEvent.click(within(nav).getByRole("button", { name: section }));
  await screen.findByRole("heading", { level: 1, name: section });
}

/** Moves to another section while Settings is open. */
function settingsSection(section: string) {
  const nav = screen.getByRole("navigation", { name: "Settings sections" });
  fireEvent.click(within(nav).getByRole("button", { name: section }));
}

/** Opens a sidebar chat's menu with the keyboard (Radix opens menus on Enter). */
async function chatMenu(title: string) {
  const trigger = await screen.findByRole("button", { name: `More actions for “${title}”` });
  trigger.focus();
  fireEvent.keyDown(trigger, { key: "Enter" });
  await screen.findByRole("menu");
  return trigger;
}

async function chatMenuItem(title: string, item: string) {
  await chatMenu(title);
  fireEvent.click(await screen.findByRole("menuitem", { name: item }));
}

/** In-memory stand-in for the Rust commands. */
function fakeApi() {
  const launch = { result: null as UpdateCheck | null, mode: "off" as UpdateMode };
  const convs: Conversation[] = [];
  const trash: Conversation[] = [];
  const projects: Project[] = [];
  const deletedProjects: Project[] = [];
  const files: ProjectFile[] = [];
  const removedFiles: ProjectFile[] = [];
  const created: (string | null)[] = [];
  const project = {
    failList: false,
    pick: [] as { name: string; size: number }[],
    skipped: [] as { name: string; reason: string }[],
    updates: [] as [string, string, string][],
    moves: [] as [string, string | null][],
    renames: [] as [string, string][],
  };
  let approvalSink: ((e: ToolEvent) => void) | null = null;
  const responses: [string, string][] = [];
  const resolved: [string, boolean][] = [];
  const unknowns: UnknownOutcome[] = [];
  let localSink: ((e: LocalModelEvent) => void) | null = null;
  const downloads: string[] = [];
  const pauses: string[] = [];
  const chatSkills = new Map<string, string | null>();
  const answerSkills: { message_id: string; name: string; label: string }[] = [];
  const localRows: LocalModelRow[] = [
    {
      id: "m1",
      origin: "catalog",
      location: null,
      model_ref: "local:m1",
      name: "Tiny Model",
      repo: "org/tiny",
      quant: "Q4_K_M",
      size: 2_000_000_000,
      params_b: 1,
      capabilities: ["chat"],
      license: "cc-by-nc-4.0",
      license_warning: true,
      card_url: "https://huggingface.co/org/tiny",
      notes: "",
      state: { state: "not_downloaded" },
      fit: { status: "caution", estimated: true, reason: "Fits, with little memory left for other apps.", suggested_quant: null },
      traits: null,
      remove_blocked: null,
    },
  ];
  const msgs = new Map<string, StoredMessage[]>();
  const artifacts: ArtifactDetail[] = [];
  // As the backend leaves it: the only chat model filled the empty Chat slot (#219).
  let settings: Settings = { slots: { chat: "local:dev-fake" }, slots_filled: ["chat"], telemetry_enabled: false };
  let n = 0;
  const reports: [string, string | null][] = [];
  const links: string[] = [];
  let listener: ((ev: ChatEvent) => void) | null = null;
  const id = () => `id-${++n}`;
  const m = (cid: string, role: "user" | "assistant", text: string, status: StoredMessage["status"]): StoredMessage => ({
    id: id(),
    conversation_id: cid,
    role,
    content: text === "" ? [] : [{ type: "text", text }],
    tool_calls: [],
    tool_call_id: null,
    model: "local:dev-fake",
    status,
    finish_reason: null,
    error: null,
    created_at: n,
    updated_at: n,
  });
  const catalog = (): ModelCatalog => ({
    slots: [{ slot_id: "chat", label: "Chat", description: "Answers", capability: "chat" }],
    models: [
      { model_ref: "local:dev-fake", label: "Dev fake", source: "dev", capabilities: ["chat"], available: true, note: null },
    ],
    providers: [{ id: "openai", name: "OpenAI", key: "none" }],
    remote_servers: [],
    local_models: localRows,
    key_store: "memory",
    keychain: { state: "ok" },
    settings,
    chat_model: null,
    chat_model_tools: true,
    compute: null,
  });
  const api: Api = {
    // The Windows artifact webview has no browser equivalent.
    artifactChildOpen: async () => {
      throw { code: "invalid", message: "the artifact webview is only used on Windows" };
    },
    artifactChildRender: async () => null,
    artifactChildBounds: async () => null,
    artifactChildFocus: async () => null,
    artifactChildClose: async () => null,
    messageSources: async () => null,
    knowledgeStatus: async () => ({ offer: "ask", model: null, embedding: null }),
    knowledgeOffer: async () => null,
    appInfo: async () => ({ version: "0.1.0", dev_fake_model: true, data_dir: "/tmp", models_dir: "/tmp/models", updates: launch.mode }),
    checkForUpdate: async () => ({ status: "up_to_date", current: "0.1.0" }) as const,
    installUpdate: async () => null,
    launchUpdateCheck: async () => launch.result,
    hubSearch: async () => ({ items: [], next: null }),
    hubRepo: async () => {
      throw { code: "model_unavailable", message: "not in this test" };
    },
    hubPull: async () => "local:x",
    hubCancel: async () => true,
    hubPullStatus: async () => ({}),
    watchAddLinks: async () => null,
    addLinkDone: async () => null,
    hubResolveLink: async () => {
      throw { code: "model_unavailable", message: "not in this test" };
    },
    hfTokenStatus: async () => false,
    setHfToken: async () => true,
    deleteHfToken: async () => false,
    listMcpServers: async () => ({ servers: [], local_servers_allowed: true }),
    saveMcpServer: async () => ({ servers: [], local_servers_allowed: true }),
    removeMcpServer: async () => ({ servers: [], local_servers_allowed: true }),
    connectCatalogue: async () => ({ entries: [] }),
    connectService: async () => ({ entries: [] }),
    restoreMcpServer: async () => ({ servers: [], local_servers_allowed: true }),
    reconnectMcpServer: async () => null,
    signInMcpServer: async () => null,
    signOutMcpServer: async () => ({ servers: [], local_servers_allowed: true }),
    listMcpTools: async (serverId) => ({ server_id: serverId, connected: false, tools: [] }),
    setMcpToolChoice: async (serverId) => ({ server_id: serverId, connected: false, tools: [] }),
    markMcpToolsReviewed: async (serverId) => ({ server_id: serverId, connected: false, tools: [] }),
    openLink: async (url) => {
      links.push(url);
      return true;
    },
    reportProblem: async (kind, messageId) => {
      reports.push([kind, messageId ?? null]);
      return null;
    },
    localDataSummary: async () => ({
      conversations: convs.length,
      artifacts: artifacts.length,
      data_dir: "/tmp",
      models_dir: "/tmp/models",
      models_bytes: 4_200_000_000,
      projects: projects.length,
      keychain: { entries: ["OpenAI key"], problem: null },
    }),
    deleteAllLocalData: async () => {
      convs.length = 0;
      return null;
    },
    listConversations: async () => [...convs],
    createConversation: async (projectId = null) => {
      created.push(projectId);
      const c = { id: id(), title: "New chat", model: null, project_id: projectId, created_at: n, updated_at: n };
      convs.unshift(c);
      msgs.set(c.id, []);
      return c;
    },
    renameConversation: async (cid, title) => {
      project.renames.push([cid, title]);
      const c = convs.find((x) => x.id === cid);
      if (c) c.title = title;
      return null;
    },
    listProjects: async () => {
      if (project.failList) throw { code: "storage", message: "Could not read or write local data." };
      return projects.map((p) => ({
        ...p,
        chats: convs.filter((c) => c.project_id === p.id).length,
        files: files.filter((f) => f.project_id === p.id).length,
      }));
    },
    createProject: async (name) => {
      const p = { id: id(), name, instructions: "", created_at: n, updated_at: n };
      projects.push(p);
      return p;
    },
    updateProject: async (pid, name, instructions) => {
      project.updates.push([pid, name, instructions]);
      const p = projects.find((x) => x.id === pid);
      if (!p) throw { code: "not_found", message: "gone" };
      Object.assign(p, { name, instructions });
      return { ...p };
    },
    deleteProject: async (pid) => {
      const i = projects.findIndex((x) => x.id === pid);
      const [p] = projects.splice(i, 1);
      if (p) deletedProjects.push(p);
      const inside = convs.filter((c) => c.project_id === pid);
      for (const c of inside) {
        convs.splice(convs.indexOf(c), 1);
        trash.push(c);
      }
      return inside.length;
    },
    restoreProject: async (pid) => {
      const i = deletedProjects.findIndex((x) => x.id === pid);
      const [p] = deletedProjects.splice(i, 1);
      if (p) projects.push(p);
      for (const c of trash.filter((x) => x.project_id === pid)) {
        trash.splice(trash.indexOf(c), 1);
        convs.unshift(c);
      }
      return null;
    },
    moveConversation: async (cid, pid) => {
      project.moves.push([cid, pid]);
      const c = convs.find((x) => x.id === cid);
      if (!c) throw { code: "not_found", message: "conversation" };
      if (pid !== null && !projects.some((x) => x.id === pid)) throw { code: "not_found", message: "This project was deleted." };
      const before = c.project_id;
      c.project_id = pid;
      return before;
    },
    listProjectFiles: async (pid) => files.filter((f) => f.project_id === pid),
    addProjectFiles: async (pid) => {
      const added = project.pick.map((x) => ({
        id: id(),
        project_id: pid,
        name: x.name,
        media_type: "text/plain",
        size: x.size,
        sha256: "0".repeat(64),
        source_path: `/home/me/${x.name}`,
        created_at: n,
        updated_at: n,
      }));
      files.push(...added);
      return { cancelled: project.pick.length === 0 && project.skipped.length === 0, added, skipped: project.skipped };
    },
    removeProjectFile: async (_pid, fid) => {
      const i = files.findIndex((f) => f.id === fid);
      const [f] = files.splice(i, 1);
      if (f) removedFiles.push(f);
      return null;
    },
    restoreProjectFile: async (_pid, fid) => {
      const i = removedFiles.findIndex((f) => f.id === fid);
      const [f] = removedFiles.splice(i, 1);
      if (f) files.push(f);
      return null;
    },
    deleteConversation: async (cid) => {
      const i = convs.findIndex((c) => c.id === cid);
      const [c] = convs.splice(i, 1);
      if (c) trash.push(c);
      return null;
    },
    restoreConversation: async (cid) => {
      const i = trash.findIndex((c) => c.id === cid);
      const [c] = trash.splice(i, 1);
      if (c) convs.unshift(c);
      return null;
    },
    listMessages: async (cid) => [...(msgs.get(cid) ?? [])],
    listSkills: async () => [
      { name: "proofread", label: "Proofread", description: "Fixes spelling and grammar.", allowed_tools: [], source: "builtin" },
      { name: "explain-code", label: "Explain code", description: "Explains code.", allowed_tools: [], source: "builtin" },
    ],
    getChatSkill: async (cid) => chatSkills.get(cid) ?? null,
    listAnswerSkills: async (cid) => answerSkills.filter((a) => msgs.get(cid)?.some((x) => x.id === a.message_id)),
    skillsOverview: async () => ({ skills: [], problems: [], folder: "/tmp/skills" }),
    reloadSkills: async () => ({ skills: [], problems: [], folder: "/tmp/skills" }),
    openSkillsFolder: async () => null,
    setChatSkill: async (cid, skill) => {
      chatSkills.set(cid, skill);
      return null;
    },
    sendMessage: async (cid, text, onEvent) => {
      listener = onEvent;
      const u = m(cid, "user", text, "complete");
      const a = m(cid, "assistant", "", "streaming");
      msgs.get(cid)?.push(u, a);
      const c = convs.find((x) => x.id === cid);
      if (c) c.title = text;
      const sk = chatSkills.get(cid);
      return { user_message: u, assistant_message: a, title: text, skill: sk === "proofread" ? "Proofread" : null };
    },
    regenerate: async () => {
      throw { code: "invalid_input", message: "Nothing to regenerate." };
    },
    stopGeneration: async () => true,
    getSettings: async () => settings,
    updateSettings: async (s) => {
      settings = s;
      return s;
    },
    listModels: async () => catalog(),
    checkKeys: async () => catalog(),
    setProviderKey: async () => catalog(),
    addRemoteServer: async () => catalog(),
    refreshRemoteServer: async () => catalog(),
    removeRemoteServer: async () => catalog(),
    restoreRemoteServer: async () => catalog(),
    checkRemoteServer: async () => ({ state: "connected", models: 0 }) as const,
    setRemoteModelTools: async () => catalog(),
    deleteProviderKey: async () => catalog(),
    restoreProviderKey: async () => catalog(),
    watchLocalModels: async (onEvent) => {
      localSink = onEvent;
      return localRows;
    },
    downloadModel: async (id) => {
      downloads.push(id);
      return null;
    },
    pauseDownload: async (id) => {
      pauses.push(id);
      return null;
    },
    removeLocalModel: async () => null,
    undoRemoveLocalModel: async () => null,
    cancelDownload: async () => null,
    checkModelTools: async () => ({ kind: "chat", tool_support: "supported", source: "checked", label: "Chat · Tools" }),
    watchTools: async (onEvent) => {
      approvalSink = onEvent;
      return { approvals: [], servers: [] };
    },
    respondApproval: async (requestId, decision) => {
      responses.push([requestId, decision]);
      return null;
    },
    listArtifacts: async (cid) =>
      artifacts
        .filter((a) => a.conversation_id === cid)
        .map((a) => ({ id: a.id, conversation_id: cid, artifact_type: a.artifact_type, title: a.title, latest_n: a.versions.length, created_at: "", updated_at: "" })),
    getArtifact: async (_cid, aid) => {
      const a = artifacts.find((x) => x.id === aid);
      if (!a) throw { code: "not_found", message: "gone" };
      return structuredClone(a);
    },
    rollbackArtifact: async (_cid, aid, to) => {
      const a = artifacts.find((x) => x.id === aid);
      const src = a?.versions.find((v) => v.n === to);
      if (!a || !src) throw { code: "not_found", message: "gone" };
      a.versions.push({ ...src, n: a.versions.length + 1, parent_n: to, author: "user", model: null, message_id: null });
      return structuredClone(a);
    },
    listUnknownOutcomes: async () => [...unknowns],
    resolveUnknownOutcome: async (key, happened) => {
      resolved.push([key, happened]);
      unknowns.splice(0);
      return null;
    },
  };
  return {
    api,
    launch,
    project,
    projects,
    files,
    created,
    convs,
    emit: (ev: ChatEvent) => listener?.(ev),
    msgs,
    approve: (e: ToolEvent) => approvalSink?.(e),
    responses,
    resolved,
    unknowns,
    artifacts,
    local: (e: LocalModelEvent) => localSink?.(e),
    downloads,
    pauses,
    localRows,
    reports,
    links,
    chatSkills,
    answerSkills,
  };
}

describe("App", () => {
  it("shows a card for an artifact the answer made and opens it in the pane", async () => {
    const { api, emit, artifacts } = fakeApi();
    render(<App api={api} />);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "Make an HTML artifact" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(await screen.findByRole("button", { name: "Stop answer" })).toBeTruthy();
    const conv = "id-1";
    const assistantId = "id-3";
    artifacts.push({
      id: "art-1",
      conversation_id: conv,
      artifact_type: { kind: "html" },
      title: "Fake artifact",
      versions: [
        { n: 1, content: "<h1>Version one</h1>", author: "model", parent_n: null, message_id: assistantId, created_at: "2026-11-24T10:00:00Z", model: "local:dev-fake" },
      ],
    });
    act(() => {
      emit({ type: "artifact_changed", message_id: assistantId, conversation_id: conv, artifact_id: "art-1", title: "Fake artifact", version: 1 });
      emit({ type: "text_delta", message_id: assistantId, offset: 0, text: "Made it." });
      emit({ type: "done", message_id: assistantId, finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 2 } });
    });
    const card = await screen.findByTestId("artifact-card");
    expect(card.textContent).toContain("Fake artifact");
    expect(card.textContent).toContain("HTML · v1");
    // The pane opened on it and named the model that made it.
    await waitFor(() => expect(shown.artifacts.at(-1)?.id).toBe("art-1"));
    expect(shown.madeBy.at(-1)).toBe("Dev fake");
    fireEvent.click(screen.getByRole("button", { name: "Open Fake artifact" }));
    expect(screen.getByTestId("artifact-host").hidden).toBe(false);

    // Network allowed for v1 doesn't carry over to a new version the model writes.
    shown.opts?.onAllowNetwork?.("art-1");
    expect(shown.opts?.allowNetwork?.("art-1")).toBe(true);
    const art = artifacts[0];
    if (!art) throw new Error("no artifact");
    art.versions.push({ ...art.versions[0]!, n: 2, parent_n: 1, content: "<h1>Version two</h1>" });
    act(() => {
      emit({ type: "artifact_changed", message_id: assistantId, conversation_id: conv, artifact_id: "art-1", title: "Fake artifact", version: 2 });
    });
    await waitFor(() => expect(shown.artifacts.at(-1)?.versions.length).toBe(2));
    expect(shown.opts?.allowNetwork?.("art-1")).toBe(false);
  });

  it("starts with the artifact pane closed when the window is too narrow to show it beside the chat", async () => {
    const matchMedia = vi.fn((q: string) => ({ matches: q === "(max-width: 1023px)" }) as MediaQueryList);
    vi.stubGlobal("matchMedia", matchMedia);
    try {
      const { api } = fakeApi();
      render(<App api={api} />);
      expect(await screen.findByText("What can I help with?")).toBeTruthy();
      expect(screen.queryByTestId("artifact-pane")).toBeNull();
      fireEvent.click(screen.getByTestId("new-chat"));
      fireEvent.click(screen.getByRole("button", { name: "Show artifacts" }));
      expect(screen.getByTestId("artifact-pane")).toBeTruthy();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("sends a message and renders the streamed answer as markdown", async () => {
    const { api, emit } = fakeApi();
    render(<App api={api} />);
    expect(await screen.findByText("What can I help with?")).toBeTruthy();
    expect(screen.getByTestId("artifact-pane")).toBeTruthy();
    expect(screen.getByText("No chats yet")).toBeTruthy();

    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "Hello" } });
    fireEvent.keyDown(box, { key: "Enter" });

    expect(await screen.findByRole("button", { name: "Stop answer" })).toBeTruthy();
    const assistantId = "id-3";
    act(() => {
      emit({ type: "text_delta", message_id: assistantId, offset: 0, text: "Hi **there**" });
    });
    expect((await screen.findByText("there")).tagName).toBe("STRONG");
    act(() => {
      emit({ type: "done", message_id: assistantId, finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 2 } });
    });
    expect(await screen.findByRole("button", { name: "Retry" })).toBeTruthy();
    await waitFor(() => expect(screen.getByRole("button", { name: "Hello" })).toBeTruthy());
    // The model badge says where the answer ran.
    expect(screen.getAllByText(/On this device/).length).toBeGreaterThan(0);
  });

  it("picks a skill in the composer with the keyboard and keeps it with the new chat", async () => {
    const { api, chatSkills } = fakeApi();
    render(<App api={api} />);
    const picker = await screen.findByRole("combobox", { name: "Skill" });
    expect(picker.textContent).toContain("No skill");
    expect(picker.getAttribute("aria-describedby")).toBe("composer-skill-d");
    picker.focus();
    fireEvent.keyDown(picker, { key: "ArrowDown" });
    fireEvent.click(await screen.findByRole("option", { name: /^Proofread/ }));
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Skill" }).textContent).toContain("Proofread"));
    expect(document.getElementById("composer-skill-d")?.textContent).toContain("never skips a tool approval");

    // The chat doesn't exist yet: the first message creates it with the chosen skill.
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "Check this" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(chatSkills.get("id-1")).toBe("proofread"));


    // Switching back to no skill saves at once for an existing chat.
    fireEvent.keyDown(screen.getByRole("combobox", { name: "Skill" }), { key: "ArrowDown" });
    fireEvent.click(await screen.findByRole("option", { name: "No skill" }));
    await waitFor(() => expect(chatSkills.get("id-1")).toBeNull());
  });

  it("discloses AI answers: first-run notice, composer line, labelled answers", async () => {
    const { api, emit } = fakeApi();
    let saved: Settings | null = null;
    const update = api.updateSettings;
    api.updateSettings = async (s) => {
      saved = s;
      return update(s);
    };
    render(<App api={api} />);
    expect(await screen.findByText(/Demios lets you chat with AI models you choose/)).toBeTruthy();
    // The persistent line is part of the composer's description for screen readers.
    const box = screen.getByLabelText("Message");
    expect(box.getAttribute("aria-describedby")).toContain("composer-disclosure");
    expect(document.getElementById("composer-disclosure")?.textContent).toContain("can be wrong");

    fireEvent.click(screen.getByRole("button", { name: "Got it" }));
    await waitFor(() => expect(screen.queryByText(/Demios lets you chat with AI models you choose/)).toBeNull());
    expect(document.activeElement).toBe(box);
    expect(saved).toMatchObject({ ai_notice_seen: true });
    // The line by the composer stays.
    expect(document.getElementById("composer-disclosure")).toBeTruthy();

    fireEvent.change(box, { target: { value: "Hello" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await screen.findByRole("button", { name: "Stop answer" });
    act(() => {
      emit({ type: "text_delta", message_id: "id-3", offset: 0, text: "Hi" });
      emit({ type: "done", message_id: "id-3", finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 1 } });
    });
    const answer = await screen.findByTestId("message-assistant");
    expect(answer.getAttribute("data-ai-generated")).toBe("true");
    expect(answer.getAttribute("data-generator")).toBe("local:dev-fake");
    expect(answer.getAttribute("aria-label")).toMatch(/^AI answer from /);
  });

  it("reports a problem or an answer without sending chat content", async () => {
    const { api, emit, reports, msgs } = fakeApi();
    render(<App api={api} />);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "private question" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await screen.findByRole("button", { name: "Stop answer" });
    // The backend stores the finished answer; the list reloads from it.
    const stored = [...msgs.values()].flat().find((x) => x.role === "assistant");
    if (stored) {
      stored.content = [{ type: "text", text: "an answer" }];
      stored.status = "complete";
    }
    act(() => {
      emit({ type: "text_delta", message_id: "id-3", offset: 0, text: "an answer" });
      emit({ type: "done", message_id: "id-3", finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 1 } });
    });
    // Wait until the chat has settled (the list reloads after the answer ends).
    await waitFor(() => expect(screen.getByRole("button", { name: "private question" })).toBeTruthy());
    fireEvent.click(await screen.findByRole("button", { name: "Report this answer" }));
    await waitFor(() => expect(reports).toEqual([["answer", "id-3"]]));
    expect(await screen.findByText("Opened the report form in your browser")).toBeTruthy();

    await openSettings("General");
    fireEvent.click(await screen.findByRole("button", { name: "Report a problem" }));
    await waitFor(() => expect(reports).toEqual([["answer", "id-3"], ["problem", null]]));
    // Only kinds and message ids went to the backend: no chat text.
    expect(JSON.stringify(reports)).not.toContain("private question");
  });

  it("a demios:// link opens the add-model confirmation and downloads nothing by itself", async () => {
    const { api } = fakeApi();
    const calls: string[] = [];
    api.watchAddLinks = async () => ({ repo: "org/Tiny-GGUF", file: null, quant: "Q8_0", revision: null });
    api.hubResolveLink = async (link) => {
      calls.push(`resolve ${link.repo} ${link.quant ?? ""}`);
      return {
        id: "org/Tiny-GGUF",
        sha: "c".repeat(40),
        gated: false,
        license: { id: "apache-2.0", name: null, link: null, warning: null },
        card_url: "https://huggingface.co/org/Tiny-GGUF",
        selected: "Tiny-Q8_0",
        fit_unknown: false,
        traits: null,
        variants: [
          { name: "Tiny-Q4_K_M", quant: "Q4_K_M", total_size: 1e9, parts: 1, verifiable: true, fit: null, downloaded: null, progress: null },
          { name: "Tiny-Q8_0", quant: "Q8_0", total_size: 2e9, parts: 1, verifiable: true, fit: null, downloaded: null, progress: null },
        ],
      };
    };
    api.hubPull = async () => {
      calls.push("pull");
      return "local:x";
    };
    api.addLinkDone = async () => {
      calls.push("done");
      return null;
    };
    render(<App api={api} />);
    expect(await screen.findByText(/A link asked to add this model/)).toBeTruthy();
    const variants = await screen.findAllByTestId("hub-variant");
    expect(variants[0]!.textContent).toContain("Chosen by the link");
    expect(variants[0]!.getAttribute("data-selected")).toBe("true");
    expect(calls).toEqual(["resolve org/Tiny-GGUF Q8_0"]);
    expect(calls).not.toContain("pull");

    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    await waitFor(() => expect(calls).toContain("done"));
    expect(screen.queryByText(/A link asked to add this model/)).toBeNull();
    expect(calls).not.toContain("pull");
  });

  it("a link mid-chat keeps the composer draft", async () => {
    const { api } = fakeApi();
    let deliver: ((l: AddModelLink) => void) | null = null;
    api.watchAddLinks = async (on) => {
      deliver = on;
      return null;
    };
    api.hubResolveLink = async () => {
      throw { code: "model_unavailable", message: "offline in this test" };
    };
    render(<App api={api} />);
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "half-written question" } });
    await waitFor(() => expect(deliver).not.toBeNull());
    act(() => deliver?.({ repo: "org/Tiny-GGUF", file: null, quant: null, revision: null }));
    fireEvent.click(await screen.findByRole("button", { name: "Not now" }));
    fireEvent.click(screen.getAllByRole("button", { name: /New chat/ })[0]!);
    const back = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    expect(back.value).toBe("half-written question");
  });

  it("About opens the privacy policy and accessibility statement through the link confirm", async () => {
    const { api, links } = fakeApi();
    let fail = false;
    api.openLink = async (url) => {
      links.push(url);
      if (fail) throw { code: "unavailable", message: "Couldn't open your browser." };
      return false; // the user cancelled the native confirm
    };
    render(<App api={api} />);
    await openSettings("General");
    fireEvent.click(await screen.findByRole("button", { name: "Privacy policy" }));
    fireEvent.click(await screen.findByRole("button", { name: "Accessibility statement" }));
    await waitFor(() => expect(links).toEqual(["https://demios.io/privacy", "https://demios.io/accessibility"]));
    expect(screen.queryByText(/Couldn't open the page/)).toBeNull();
    expect(screen.queryByRole("button", { name: /Impressum|Imprint|Legal notice/ })).toBeNull();
    fail = true;
    fireEvent.click(screen.getByRole("button", { name: "Privacy policy" }));
    expect(await screen.findByText("Couldn't open the page. Couldn't open your browser.")).toBeTruthy();
  });

  it("links in answers go to the backend to confirm and open, never navigate the app", async () => {
    const { api, emit, msgs, links } = fakeApi();
    let refuse = false;
    api.openLink = async (url) => {
      links.push(url);
      if (refuse) throw { code: "invalid_input", message: "Only web links (http and https) open from answers." };
      return true;
    };
    render(<App api={api} />);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "Where?" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await screen.findByRole("button", { name: "Stop answer" });
    const text =
      "See [the docs](https://example.com/docs) or [mail us](mailto:a@example.com), https://auto.example/x and [rel](/x).";
    const stored = [...msgs.values()].flat().find((x) => x.role === "assistant");
    if (stored) {
      stored.content = [{ type: "text", text }];
      stored.status = "complete";
    }
    act(() => {
      emit({ type: "text_delta", message_id: "id-3", offset: 0, text });
      emit({ type: "done", message_id: "id-3", finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 1 } });
    });
    await waitFor(() => expect(screen.getByRole("button", { name: "Where?" })).toBeTruthy());
    const before = window.location.href;
    const docs = await screen.findByRole("link", { name: "the docs" });
    const notPrevented = fireEvent.click(docs);
    expect(notPrevented).toBe(false); // default navigation cancelled
    expect(links).toEqual(["https://example.com/docs"]);
    expect(window.location.href).toBe(before);

    // Ctrl/Cmd-click is a normal click: same confirmed path, no new window.
    expect(fireEvent.click(docs, { ctrlKey: true })).toBe(false);
    expect(fireEvent.click(docs, { metaKey: true })).toBe(false);
    expect(links).toEqual(["https://example.com/docs", "https://example.com/docs", "https://example.com/docs"]);
    // Middle-click and drag can't bypass the confirmation.
    expect(fireEvent(docs, new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 }))).toBe(false);
    expect(docs.getAttribute("draggable")).toBe("false");
    expect(fireEvent.dragStart(docs)).toBe(false);
    expect(links.length).toBe(3);
    // A bare URL is autolinked and takes the same path.
    fireEvent.click(screen.getByRole("link", { name: "https://auto.example/x" }));
    expect(links[3]).toBe("https://auto.example/x");
    // A relative link loses its href when sanitised: not a link, clicking does nothing.
    expect(screen.queryByRole("link", { name: "rel" })).toBeNull();
    fireEvent.click(screen.getByText("rel"));
    expect(links.length).toBe(4);
    links.splice(0);

    // Only http(s) opens: the backend refuses anything else and the user is told why.
    refuse = true;
    fireEvent.click(screen.getByRole("link", { name: "mail us" }));
    expect(links[0]).toBe("mailto:a@example.com");
    expect(await screen.findByText("Only web links (http and https) open from answers.")).toBeTruthy();
  });

  it("does not show the first-run notice once seen", async () => {
    const { api } = fakeApi();
    await api.updateSettings({ slots: {}, telemetry_enabled: false, ai_notice_seen: true });
    render(<App api={api} />);
    expect(await screen.findByText("What can I help with?")).toBeTruthy();
    expect(screen.queryByText(/Demios lets you chat with AI models you choose/)).toBeNull();
    expect(document.getElementById("composer-disclosure")).toBeTruthy();
  });

  it("keeps the prompt when sending fails", async () => {
    const { api } = fakeApi();
    api.sendMessage = async () => {
      throw { code: "busy", message: "An answer is already streaming in this chat." };
    };
    render(<App api={api} />);
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "keep me" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(await screen.findByText("An answer is already streaming in this chat.")).toBeTruthy();
    expect(box.value).toBe("keep me");
  });

  it("deletes a chat at once and restores it with undo", async () => {
    const { api } = fakeApi();
    await api.createConversation();
    let deletes = 0;
    let restores = 0;
    const del = api.deleteConversation;
    const res = api.restoreConversation;
    api.deleteConversation = async (id) => {
      deletes++;
      return del(id);
    };
    api.restoreConversation = async (id) => {
      restores++;
      return res(id);
    };
    render(<App api={api} />);
    await chatMenuItem("New chat", "Delete");
    expect(await screen.findByText("Deleted “New chat”")).toBeTruthy();
    expect(deletes).toBe(1);
    await waitFor(() => expect(screen.queryByRole("button", { name: "More actions for “New chat”" })).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(await screen.findByRole("button", { name: "More actions for “New chat”" })).toBeTruthy();
    expect(restores).toBe(1);
    expect(deletes).toBe(1);
  });

  it("the Project search slot is a picker like Chat; while empty it says search uses keywords", async () => {
    const { api } = fakeApi();
    const list = api.listModels;
    api.listModels = async () => {
      const c = await list();
      const slot = { slot_id: "embed", label: "Project search", description: "Finds passages by meaning.", capability: "embedding" as const };
      const v2 = { model_ref: "local:nomic-embed-text-v2-moe-q5_k_m", label: "Nomic Embed Text v2 MoE · Q5_K_M", source: "local" as const, capabilities: ["embedding" as const], available: true, note: null };
      return { ...c, slots: [...c.slots, slot], models: [...c.models, v2] };
    };
    render(<App api={api} />);
    await openSettings("Models");
    const picker = await screen.findByRole("combobox", { name: "Project search" });
    expect(picker.textContent).toContain("Not set");
    expect(screen.getByText("No model chosen: project files are searched by keywords.")).toBeTruthy();
    // No separate offer or card: the model is downloaded and chosen like any other.
    expect(screen.queryByText(/Recommended:/)).toBeNull();
    expect(screen.queryByRole("heading", { name: "Project search" })).toBeNull();
    picker.focus();
    fireEvent.keyDown(picker, { key: "ArrowDown" });
    fireEvent.click(await screen.findByRole("option", { name: "Nomic Embed Text v2 MoE · Q5_K_M" }));
    await waitFor(() => expect(screen.queryByText("No model chosen: project files are searched by keywords.")).toBeNull());
    expect((await api.getSettings()).slots["embed"]).toBe("local:nomic-embed-text-v2-moe-q5_k_m");
    // One line in the select; the full name is its tooltip and stays in the text.
    const chosen = screen.getByRole("combobox", { name: "Project search" });
    expect(chosen.getAttribute("title")).toBe("Nomic Embed Text v2 MoE · Q5_K_M");
    expect(chosen.querySelector(".dn-select-value")?.textContent).toBe("Nomic Embed Text v2 MoE · Q5_K_M");
  });

  it("recommended catalogue models carry a Recommended chip, from the catalogue's flag", async () => {
    const f = fakeApi();
    f.localRows[0]!.recommended = true;
    render(<App api={f.api} />);
    await openSettings("Models");
    const rows = await screen.findAllByTestId("local-model-row");
    expect(within(rows[0]!).getByText("Recommended")).toBeTruthy();
    expect(rows.slice(1).every((r) => within(r).queryByText("Recommended") === null)).toBe(true);
  });

  it("several chat models and no choice: the chat asks for one, never an automatic pick", async () => {
    const { api } = fakeApi();
    await api.updateSettings({ slots: {}, telemetry_enabled: false, ai_notice_seen: true });
    const list = api.listModels;
    api.listModels = async () => {
      const c = await list();
      const other = { model_ref: "local:m1", label: "Tiny Model · Q4_K_M", source: "local" as const, capabilities: ["chat" as const], available: true, note: null };
      return { ...c, models: [...c.models, other] };
    };
    render(<App api={api} />);
    expect(await screen.findByText("Choose a model for Chat.")).toBeTruthy();
    // The notice and the composer both say so, each with a button to the picker.
    expect(screen.getByLabelText("Message").getAttribute("placeholder")).toBe("Choose a model for Chat to start chatting");
    const buttons = screen.getAllByRole("button", { name: "Choose a model" });
    expect(buttons).toHaveLength(2);
    fireEvent.click(buttons[0]!);
    const chat = await screen.findByRole("combobox", { name: "Chat" });
    expect(chat.textContent).toContain("Not set");
    expect(document.body.textContent).not.toContain("automatically");
  });

  it("'Set as default' sets the task's model, confirms it, and the row says it's the default", async () => {
    const f = fakeApi();
    const { api } = f;
    await api.updateSettings({ slots: {}, telemetry_enabled: false, ai_notice_seen: true });
    f.localRows[0]!.state = { state: "ready" };
    const list = api.listModels;
    api.listModels = async () => {
      const c = await list();
      const tiny = { model_ref: "local:m1", label: "Tiny Model · Q4_K_M", source: "local" as const, capabilities: ["chat" as const], available: true, note: null };
      const claude = { model_ref: "byok:openai:gpt-5", label: "OpenAI · gpt-5", source: "byok" as const, capabilities: ["chat" as const], available: true, note: null };
      return { ...c, models: [...c.models, tiny, claude], providers: [{ id: "openai", name: "OpenAI", key: "saved" as const }] };
    };
    render(<App api={api} />);
    await openSettings("Models");
    fireEvent.click(await screen.findByRole("button", { name: "Set Tiny Model as default for Chat" }));
    expect(await screen.findByText("Now used for Chat")).toBeTruthy();
    expect((await api.getSettings()).slots["chat"]).toBe("local:m1");
    expect(await screen.findByText("Default for Chat")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Set Tiny Model as default for Chat" })).toBeNull();
    // A provider's models, once its key is saved, have it too.
    fireEvent.click(screen.getByRole("tab", { name: "Providers" }));
    fireEvent.click(await screen.findByRole("button", { name: "Set gpt-5 as default for Chat" }));
    await waitFor(async () => expect((await api.getSettings()).slots["chat"]).toBe("byok:openai:gpt-5"));
  });

  it("shows models and settings with telemetry off", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    await openSettings("Models");
    expect(await screen.findByText("Model for each task")).toBeTruthy();
    expect(screen.getByLabelText("Chat")).toBeTruthy();
    // On this device is the first tab; the models folder is shown there.
    expect(screen.getByRole("tab", { name: "On this device" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByText("Downloaded models are stored in /tmp/models")).toBeTruthy();
    fireEvent.click(screen.getByRole("tab", { name: "Providers" }));
    const key = (await screen.findByLabelText("OpenAI")) as HTMLInputElement;
    expect(key.type).toBe("password");
    // The data-flow sentence once for all providers, not under every key.
    expect(screen.getAllByText(/sent to that provider under your account/)).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Save key" }));
    expect(await screen.findByText("Paste the key first.")).toBeTruthy();
    expect(key.getAttribute("aria-invalid")).toBe("true");

    settingsSection("General");
    expect(await screen.findByRole("heading", { level: 1, name: "General" })).toBeTruthy();
    expect(screen.getByText(/Read-only tools run without asking/)).toBeTruthy();
    // No inert telemetry switch; a plain statement instead (T-STORE-11).
    expect(await screen.findByText(/Demios collects no usage data/)).toBeTruthy();
    expect(screen.queryByRole("switch")).toBeNull();
    expect(screen.getByText("Where your data goes")).toBeTruthy();
  });

  it("deletes all local data only after a confirm, and cancel changes nothing", async () => {
    const { api } = fakeApi();
    let calls = 0;
    let fail = true;
    api.deleteAllLocalData = async () => {
      calls++;
      if (fail) throw { code: "partial", message: "Some data could not be deleted: keys in the system keychain. Try again." };
      return null;
    };
    render(<App api={api} />);
    await openSettings("General");
    const trigger = await screen.findByRole("button", { name: "Delete all local data" });

    fireEvent.click(trigger);
    const dialog = await screen.findByRole("alertdialog", { name: "Delete all local data?" });
    expect(dialog.textContent).toContain("can't be undone");
    await waitFor(() => expect(dialog.textContent).toContain("4.2 GB"));
    expect(dialog.textContent).toContain("/tmp/models");
    expect(dialog.textContent).toContain("OpenAI key");
    // Focus starts on Cancel; the red button is armed only after a short guard.
    expect(document.activeElement?.textContent).toBe("Cancel");
    const confirm = screen.getByRole("button", { name: "Delete all data" }) as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    fireEvent.click(confirm);
    expect(calls).toBe(0);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(calls).toBe(0);
    await waitFor(() => expect(document.activeElement).toBe(trigger));

    // Esc closes it too, and deletes nothing.
    fireEvent.click(trigger);
    const again = await screen.findByRole("alertdialog");
    fireEvent.keyDown(again, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(calls).toBe(0);

    fireEvent.click(trigger);
    const armed = (await screen.findByRole("button", { name: "Delete all data" })) as HTMLButtonElement;
    await waitFor(() => expect(armed.disabled).toBe(false), { timeout: 1500 });
    fireEvent.click(armed);
    // A partial failure stays in the dialog and says what is left.
    expect(await screen.findByText(/keys in the system keychain/)).toBeTruthy();
    expect(screen.getByRole("alertdialog")).toBeTruthy();

    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Delete all data" }));
    expect(await screen.findByText("All local data deleted")).toBeTruthy();
    expect(calls).toBe(2);
  });

  it("keeps the delete button disabled until it knows what will be deleted", async () => {
    const { api } = fakeApi();
    let finish: (() => void) | null = null;
    const summary = api.localDataSummary;
    api.localDataSummary = () =>
      new Promise((resolve) => {
        finish = () => void summary().then(resolve);
      });
    render(<App api={api} />);
    await openSettings("General");
    fireEvent.click(await screen.findByRole("button", { name: "Delete all local data" }));
    const confirm = (await screen.findByRole("button", { name: "Delete all data" })) as HTMLButtonElement;
    // Well past the 500 ms guard, still disabled: the list isn't known yet.
    await new Promise((r) => setTimeout(r, 700));
    expect(confirm.disabled).toBe(true);
    act(() => finish?.());
    await waitFor(() => expect(confirm.disabled).toBe(false));
  });

  it("shows the build channel and commit on the About screen", async () => {
    const { api } = fakeApi();
    api.appInfo = async () => ({
      version: "0.1.0",
      dev_fake_model: false,
      data_dir: null,
      models_dir: null,
      commit: "1a2b3c4",
      channel: "preview",
      updates: "off",
    });
    render(<App api={api} />);
    await openSettings("General");
    expect(await screen.findByText("Preview build (unsigned), commit 1a2b3c4")).toBeTruthy();
  });

  it("asks to connect a model when none is available", async () => {
    const { api } = fakeApi();
    api.appInfo = async () => ({ version: "0.1.0", dev_fake_model: false, data_dir: null, models_dir: null, updates: "off" });
    render(<App api={api} />);
    expect(await screen.findByText(/No model connected/)).toBeTruthy();
    expect((screen.getByLabelText("Message") as HTMLTextAreaElement).disabled).toBe(true);
    expect(screen.getAllByRole("button", { name: "Connect a model" }).length).toBeGreaterThan(0);
    // The button is a deep link: it opens Models › On this device, even if another page was last.
    localStorage.setItem("dn.settings", JSON.stringify({ section: "skills" }));
    fireEvent.click(screen.getAllByRole("button", { name: "Connect a model" })[0]!);
    expect(await screen.findByRole("heading", { level: 1, name: "Models" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "On this device" }).getAttribute("aria-selected")).toBe("true");
  });

  it("shows an approval card with the exact arguments and never auto-allows", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    const prompt = (risk: ApprovalPrompt["risk"]): ApprovalPrompt => ({
      request_id: `r-${risk}`,
      scope: `conv:${c.id}`,
      capability_id: "mcp.files.write_file",
      title: "Write to notes/todo.md",
      description: "Replaces the file.",
      risk,
      policy: "ask_once_per_conversation",
      arguments: { path: "notes/todo.md", content: "milk" },
      requester: { kind: "model" },
    });
    act(() => f.approve({ type: "requested", prompt: prompt("low_write") }));
    const card = await screen.findByTestId("approval-card");
    expect(card.textContent).toContain('"path": "notes/todo.md"');
    expect(screen.getByText("Changes local data")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Always allow in this chat/ })).toBeTruthy();
    expect(f.responses).toEqual([]);
    // Keys in the first 500 ms after the card appears are ignored.
    fireEvent.keyDown(card, { key: "Enter", metaKey: true, ctrlKey: true });
    expect(f.responses).toEqual([]);
    await act(() => new Promise((r) => setTimeout(r, 550)));
    // ⌘↵ / Ctrl+↵ allows once.
    fireEvent.keyDown(card, { key: "Enter", metaKey: true, ctrlKey: true });
    await waitFor(() => expect(f.responses).toEqual([["r-low_write", "approve"]]));
    await waitFor(() => expect(screen.queryByTestId("approval-card")).toBeNull());

    // Destructive: no "always allow"; Esc denies.
    act(() => f.approve({ type: "requested", prompt: prompt("destructive") }));
    const card2 = await screen.findByTestId("approval-card");
    expect(screen.queryByRole("button", { name: /Always allow in this chat/ })).toBeNull();
    await act(() => new Promise((r) => setTimeout(r, 550)));
    fireEvent.keyDown(card2, { key: "Escape" });
    await waitFor(() => expect(f.responses[1]).toEqual(["r-destructive", "deny"]));
  });

  it("asks the user to resolve an unknown outcome", async () => {
    const f = fakeApi();
    await f.api.createConversation();
    f.unknowns.push({
      key: "k1",
      capability_id: "mcp.mail.send",
      arguments: { to: "pat@example.com" },
      error: "interrupted",
      updated_at_ms: 1,
    });
    render(<App api={f.api} />);
    expect(await screen.findByText("Did this go through?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "It didn't happen" }));
    await waitFor(() => expect(f.resolved).toEqual([["k1", false]]));
    await waitFor(() => expect(screen.queryByText("Did this go through?")).toBeNull());
  });

  it("says when the chat's saved skill is no longer available instead of hiding it", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    f.chatSkills.set(c.id, "removed-skill");
    render(<App api={f.api} />);
    expect((await screen.findByTestId("skill-missing")).textContent).toContain("“removed-skill” isn't available");
    expect(document.getElementById("composer-skill-d")?.textContent).toContain("Choose another skill or No skill");
  });

  it("shows the skill each answer was written with, also after the chat's skill changed", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    const base = {
      conversation_id: c.id,
      tool_call_id: null,
      model: "local:dev-fake",
      status: "complete" as const,
      error: null,
      tool_calls: [],
      created_at: 1,
      updated_at: 1,
    };
    f.msgs.set(c.id, [
      { ...base, id: "u1", role: "user", content: [{ type: "text", text: "check this" }], finish_reason: null },
      { ...base, id: "a1", role: "assistant", content: [{ type: "text", text: "Checked." }], finish_reason: "stop" },
      { ...base, id: "u2", role: "user", content: [{ type: "text", text: "and this" }], finish_reason: null },
      { ...base, id: "a2", role: "assistant", content: [{ type: "text", text: "Done." }], finish_reason: "stop" },
    ]);
    // Only the first answer used a skill; the chat has none now.
    f.answerSkills.push({ message_id: "a1", name: "proofread", label: "Proofread" });
    render(<App api={f.api} />);
    const answers = await screen.findAllByTestId("message-assistant");
    await waitFor(() => expect(within(answers[0]!).getByTestId("answer-skill").textContent).toBe("Skill: Proofread"));
    expect(within(answers[1]!).queryByTestId("answer-skill")).toBeNull();
    expect(screen.getByRole("combobox", { name: "Skill" }).textContent).toContain("No skill");
  });

  it("renders stored tool calls as cards with their results", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    const base = {
      conversation_id: c.id,
      tool_call_id: null,
      model: "byok:anthropic:claude-sonnet-5-5",
      status: "complete" as const,
      error: null,
      created_at: 1,
      updated_at: 1,
    };
    f.msgs.set(c.id, [
      { ...base, id: "u", role: "user", content: [{ type: "text", text: "save it" }], tool_calls: [], finish_reason: null },
      {
        ...base,
        id: "a1",
        role: "assistant",
        content: [],
        tool_calls: [{ id: "call_1", name: "mcp__files__write_file", arguments: { path: "x.md" } }],
        finish_reason: "tool_calls",
      },
      { ...base, id: "t", role: "tool", tool_call_id: "call_1", content: [{ type: "text", text: "Wrote 1 file." }], tool_calls: [], finish_reason: null },
      { ...base, id: "a2", role: "assistant", content: [{ type: "text", text: "Saved." }], tool_calls: [], finish_reason: "stop" },
    ]);
    render(<App api={f.api} />);
    const card = await screen.findByTestId("tool-call-card");
    expect(card.textContent).toContain("files · write_file");
    expect(card.textContent).toContain("Done");
    expect(screen.queryByText("Wrote 1 file.")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /write_file/ }));
    expect(await screen.findByText("Wrote 1 file.")).toBeTruthy();
    expect(screen.getByText("Saved.")).toBeTruthy();
  });

  it("never approves when the user is typing in the composer as a card appears", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    box.focus();
    fireEvent.change(box, { target: { value: "next message" } });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r1",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write to notes/todo.md",
          description: "",
          risk: "low_write",
          policy: "ask_once_per_conversation",
          arguments: { path: "notes/todo.md" },
          requester: { kind: "model" },
        },
      }),
    );
    await screen.findByTestId("approval-card");
    expect(document.activeElement).toBe(box);
    fireEvent.keyDown(box, { key: "Enter", metaKey: true, ctrlKey: true });
    await act(() => new Promise((r) => setTimeout(r, 50)));
    expect(f.responses).toEqual([]);
    expect(screen.getByTestId("approval-card")).toBeTruthy();
    expect(await screen.findByText("Approval needed: Write to notes/todo.md")).toBeTruthy();
  });

  it("tells the user when an approval expired and when a tool server failed", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r2",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write",
          description: "",
          risk: "low_write",
          policy: "always_ask",
          arguments: {},
          requester: { kind: "model" },
        },
      }),
    );
    await screen.findByTestId("approval-card");
    act(() => f.approve({ type: "resolved", request_id: "r2", expired: true }));
    expect(await screen.findByText(/approval request expired/)).toBeTruthy();
    expect(screen.queryByTestId("approval-card")).toBeNull();

    act(() =>
      f.approve({
        type: "mcp_server",
        status: { server_id: "fs", name: "Files", state: "failed", message: "It didn't start.", tools: 0, review: 0, revision: 0 },
      }),
    );
    expect(await screen.findByText("Files couldn't connect. It didn't start.")).toBeTruthy();
  });

  it("ignores clicks on a card's buttons in its first half second", async () => {
    const f = fakeApi();
    const c = await f.api.createConversation();
    render(<App api={f.api} />);
    await screen.findByRole("button", { name: "New chat" });
    act(() =>
      f.approve({
        type: "requested",
        prompt: {
          request_id: "r3",
          scope: `conv:${c.id}`,
          capability_id: "mcp.files.write_file",
          title: "Write",
          description: "",
          risk: "low_write",
          policy: "always_ask",
          arguments: {},
          requester: { kind: "model" },
        },
      }),
    );
    const allow = await screen.findByRole("button", { name: /Allow once/ });
    // e.g. the second click of a double-click aimed at the previous card.
    fireEvent.click(allow);
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(f.responses).toEqual([]);
    await act(() => new Promise((r) => setTimeout(r, 550)));
    fireEvent.click(allow);
    await waitFor(() => expect(f.responses).toEqual([["r3", "approve"]]));
  });

  it("downloads a local model with progress, pause and a licence warning", async () => {
    const f = fakeApi();
    render(<App api={f.api} />);
    await openSettings("Models");
    const row = await screen.findByTestId("local-model-row");
    expect(row.textContent).toContain("Tight fit (estimate)");
    expect(row.textContent).toContain("Licence: cc-by-nc-4.0");
    expect(screen.getByText(/Check the licence before downloading/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Download model" }));
    await waitFor(() => expect(f.downloads).toEqual(["m1"]));

    const base = f.localRows[0]!;
    act(() =>
      f.local({
        type: "changed",
        row: { ...base, state: { state: "downloading", done: 500_000_000, total: 2_000_000_000, bytes_per_sec: 10_000_000 } },
      }),
    );
    const bar = await screen.findByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("500000000");
    expect(screen.getByText(/500 MB of 2.0 GB · 3 min left/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    await waitFor(() => expect(f.pauses).toEqual(["m1"]));

    act(() =>
      f.local({ type: "changed", row: { ...base, state: { state: "failed", message: "Couldn't reach Hugging Face.", done: 0, total: 2_000_000_000 } } }),
    );
    expect(await screen.findByText("Couldn't reach Hugging Face.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();

    act(() => f.local({ type: "changed", row: { ...base, state: { state: "queued", resuming: true } } }));
    expect(await screen.findByText("Checking the partial download…")).toBeTruthy();

    act(() => f.local({ type: "changed", row: { ...base, state: { state: "ready" } } }));
    expect(await screen.findByText("On this device", { selector: ".dn-badge" })).toBeTruthy();
    expect(await screen.findByText("Tiny Model downloaded")).toBeTruthy();
  });
});

describe("Launch update check (#95)", () => {
  it("shows a notice for a newer version and opens About › Updates on it", async () => {
    const { api, launch } = fakeApi();
    launch.mode = "self_update";
    launch.result = { status: "available", version: "0.2.0", current: "0.1.0", notes: "Fixes", date: null };
    let installs = 0;
    api.installUpdate = async () => {
      installs++;
      return null;
    };
    render(<App api={api} />);
    const notice = await screen.findByTestId("update-notice");
    expect(notice.textContent).toContain("Demios 0.2.0 is available.");
    fireEvent.click(screen.getByRole("button", { name: "View update" }));
    expect(await screen.findByText(/Version 0.2.0 is available/)).toBeTruthy();
    expect(screen.queryByTestId("update-notice")).toBeNull();
    expect(installs).toBe(0);
  });

  it("doesn't ask at all in builds that don't update themselves", async () => {
    const { api, launch } = fakeApi();
    launch.result = { status: "available", version: "0.2.0", current: "0.1.0", notes: "", date: null };
    let asked = 0;
    api.launchUpdateCheck = async () => {
      asked++;
      return launch.result;
    };
    render(<App api={api} />);
    await screen.findByLabelText("Message");
    expect(asked).toBe(0);
    expect(screen.queryByTestId("update-notice")).toBeNull();
  });

  it("stays silent when the launch check finds nothing or fails", async () => {
    const { api, launch } = fakeApi();
    launch.mode = "self_update";
    api.launchUpdateCheck = async () => {
      throw { code: "internal", message: "boom" };
    };
    render(<App api={api} />);
    await screen.findByLabelText("Message");
    expect(screen.queryByTestId("update-notice")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("Projects", () => {
  it("shows the chat list's error, not a guessed list, if reloading after a project delete fails", async () => {
    const { api, projects, convs } = fakeApi();
    projects.push({ id: "p1", name: "Thesis", instructions: "", created_at: 0, updated_at: 0 });
    convs.push({ id: "c1", title: "In project", model: null, project_id: "p1", created_at: 0, updated_at: 0 });
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Thesis" }));
    api.listConversations = async () => {
      throw { code: "storage", message: "Could not read or write local data." };
    };
    fireEvent.click(await screen.findByRole("button", { name: "Delete project" }));
    expect(await screen.findByText(/Couldn't load chats/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "In project" })).toBeNull();
  });

  it("lists none at first, asks for a name and opens the new project", async () => {
    const { api, projects } = fakeApi();
    render(<App api={api} />);
    expect(await screen.findByText("Group chats with their own files and instructions.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "New project" }));
    const name = await screen.findByLabelText("Name");
    expect(document.activeElement).toBe(name);
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    expect(await screen.findByText("Enter a name for the project.")).toBeTruthy();
    expect(projects).toHaveLength(0);
    fireEvent.change(name, { target: { value: "Thesis" } });
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    const heading = await screen.findByRole("heading", { level: 1, name: "Thesis" });
    await waitFor(() => expect(document.activeElement).toBe(heading));
    expect(await screen.findByRole("button", { name: "Thesis" })).toBeTruthy();
    expect(screen.getByText("No files yet.")).toBeTruthy();
  });

  it("shows an error with Retry when projects can't load", async () => {
    const { api, project } = fakeApi();
    project.failList = true;
    render(<App api={api} />);
    expect(await screen.findByText(/Couldn't load projects/)).toBeTruthy();
    project.failList = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Group chats with their own files and instructions.")).toBeTruthy();
  });

  it("keeps unsaved instructions while a chat is open, then saves them", async () => {
    const { api, project, projects } = fakeApi();
    projects.push({ id: "p1", name: "Thesis", instructions: "", created_at: 0, updated_at: 0 });
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Thesis" }));
    const box = await screen.findByLabelText("Instructions");
    fireEvent.change(box, { target: { value: "Cite sources." } });
    expect(screen.getByText("Unsaved changes")).toBeTruthy();
    // Look at a chat and come back: the draft is still there.
    fireEvent.click(screen.getByTestId("new-chat"));
    fireEvent.click(await screen.findByRole("button", { name: "Thesis" }));
    expect(((await screen.findByLabelText("Instructions")) as HTMLTextAreaElement).value).toBe("Cite sources.");
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("Project saved")).toBeTruthy();
    expect(project.updates).toEqual([["p1", "Thesis", "Cite sources."]]);
    expect(screen.queryByText("Unsaved changes")).toBeNull();
  });

  it("refuses instructions over the limit without calling the backend", async () => {
    const { api, project, projects } = fakeApi();
    projects.push({ id: "p1", name: "Thesis", instructions: "", created_at: 0, updated_at: 0 });
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Thesis" }));
    fireEvent.change(await screen.findByLabelText("Instructions"), { target: { value: "x".repeat(8001) } });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText(/Instructions can be at most 8,000 characters/)).toBeTruthy();
    expect(project.updates).toHaveLength(0);
  });

  it("starts a chat in the project on the first message", async () => {
    const { api, projects, created } = fakeApi();
    projects.push({ id: "p1", name: "Thesis", instructions: "", created_at: 0, updated_at: 0 });
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Thesis" }));
    fireEvent.click(await screen.findByRole("button", { name: "New chat" }));
    const box = await screen.findByLabelText("Message");
    expect(screen.getByText("Thesis / New chat")).toBeTruthy();
    fireEvent.change(box, { target: { value: "Outline chapter 2" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(created).toEqual(["p1"]));
    // The chat shows on the project page.
    fireEvent.click(screen.getByRole("button", { name: "Thesis" }));
    const list = await screen.findByTestId("project-chats");
    expect(list.textContent).toContain("Outline chapter 2");
  });

  it("adds files, reports skipped ones, and removes with Undo", async () => {
    const { api, project, projects, files } = fakeApi();
    projects.push({ id: "p1", name: "Thesis", instructions: "", created_at: 0, updated_at: 0 });
    project.pick = [{ name: "notes.md", size: 2048 }];
    project.skipped = [{ name: "video.mov", reason: "It's larger than 50 MB." }];
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Thesis" }));
    fireEvent.click(await screen.findByRole("button", { name: "Add files" }));
    expect(await screen.findByText("Added “notes.md”")).toBeTruthy();
    expect(screen.getByText(/video.mov: It's larger than 50 MB./)).toBeTruthy();
    expect(screen.getAllByTestId("project-file")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Remove “notes.md”" }));
    expect(await screen.findByText("Removed “notes.md”")).toBeTruthy();
    expect(screen.queryAllByTestId("project-file")).toHaveLength(0);
    expect(files).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(screen.getAllByTestId("project-file")).toHaveLength(1));
  });

  it("deletes a project with its chats and brings both back with Undo", async () => {
    const { api, projects, convs } = fakeApi();
    projects.push({ id: "p1", name: "Thesis", instructions: "", created_at: 0, updated_at: 0 });
    convs.push({ id: "c1", title: "In project", model: null, project_id: "p1", created_at: 0, updated_at: 0 });
    convs.push({ id: "c2", title: "Loose", model: null, project_id: null, created_at: 0, updated_at: 0 });
    render(<App api={api} />);
    expect(await screen.findByRole("button", { name: "In project" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Thesis" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete project" }));
    expect(await screen.findByText("Deleted “Thesis” and its chat")).toBeTruthy();
    await waitFor(() => expect(screen.queryByRole("button", { name: "In project" })).toBeNull());
    expect(screen.queryByRole("button", { name: "Thesis" })).toBeNull();
    expect(screen.getByRole("button", { name: "Loose" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(await screen.findByRole("button", { name: "In project" })).toBeTruthy();
    expect(await screen.findByRole("button", { name: "Thesis" })).toBeTruthy();
  });
});

describe("Settings quick fixes (#174 review)", () => {
  it("shows General even when the model catalogue can't load; only Models shows the error", async () => {
    const { api } = fakeApi();
    const list = api.listModels;
    let broken = true;
    api.listModels = async () => {
      if (broken) throw { code: "storage", message: "Could not read or write local data." };
      return list();
    };
    render(<App api={api} />);
    await openSettings("General");
    expect(await screen.findByRole("button", { name: "Delete all local data" })).toBeTruthy();
    expect(screen.queryByText(/Couldn't load your models/)).toBeNull();
    settingsSection("Models");
    expect(await screen.findByText(/Couldn't load your models/)).toBeTruthy();
    broken = false;
    fireEvent.click(screen.getAllByRole("button", { name: "Retry" }).at(-1)!);
    await waitFor(() => expect(screen.queryByText(/Couldn't load your models/)).toBeNull());
  });

  it("hides the artifact pane while Settings is open and brings it back for the chat", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    expect(await screen.findByTestId("artifact-pane")).toBeTruthy();
    await openSettings("General");
    expect(screen.queryByTestId("artifact-pane")).toBeNull();
    expect(screen.queryByRole("button", { name: "Hide artifacts" })).toBeNull();
    expect(document.querySelector(".app")?.classList.contains("artifacts-open")).toBe(false);
    fireEvent.click(screen.getByTestId("new-chat"));
    expect(await screen.findByTestId("artifact-pane")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Hide artifacts" })).toBeTruthy();
  });
});

describe("Move a chat into or out of a project (#136)", () => {
  const setup = () => {
    const f = fakeApi();
    f.projects.push({ id: "p1", name: "Thesis", instructions: "", created_at: 0, updated_at: 0 });
    f.projects.push({ id: "p2", name: "Work", instructions: "", created_at: 0, updated_at: 0 });
    f.convs.push({ id: "c1", title: "Loose chat", model: null, project_id: null, created_at: 0, updated_at: 0 });
    return f;
  };
  const pick = async (name: string) => {
    fireEvent.click(await screen.findByRole("combobox", { name: "Project" }));
    fireEvent.click(await screen.findByRole("option", { name }));
  };

  it("moves the open chat into a project, shows it there, and Undo moves it back", async () => {
    const { api, project, convs } = setup();
    render(<App api={api} />);
    expect(await screen.findByText("Loose chat", { selector: ".titlebar-title" })).toBeTruthy();
    await pick("Thesis");
    expect(await screen.findByText("Moved to “Thesis”")).toBeTruthy();
    expect(project.moves).toEqual([["c1", "p1"]]);
    expect(await screen.findByText("Thesis / Loose chat")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(project.moves).toEqual([["c1", "p1"], ["c1", null]]));
    await waitFor(() => expect(convs[0]?.project_id).toBeNull());
    expect(await screen.findByText("Loose chat", { selector: ".titlebar-title" })).toBeTruthy();
  });

  it("moves a chat out of its project", async () => {
    const { api, project, convs } = setup();
    convs[0]!.project_id = "p2";
    render(<App api={api} />);
    expect(await screen.findByText("Work / Loose chat")).toBeTruthy();
    await pick("No project");
    expect(await screen.findByText("Removed from “Work”")).toBeTruthy();
    expect(project.moves).toEqual([["c1", null]]);
  });

  it("says so when the project is gone, and the chat stays where it was", async () => {
    const { api, projects, convs } = setup();
    render(<App api={api} />);
    await screen.findByText("Loose chat", { selector: ".titlebar-title" });
    projects.splice(0, 1); // Thesis deleted elsewhere
    await pick("Thesis");
    expect(await screen.findByText(/Couldn't move the chat. This project was deleted./)).toBeTruthy();
    expect(convs[0]?.project_id).toBeNull();
  });

  it("a new chat starts in the project picked before the first message", async () => {
    const { api, created, convs } = setup();
    convs.length = 0;
    render(<App api={api} />);
    await pick("Work");
    expect(await screen.findByText("Work / New chat")).toBeTruthy();
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "Plan the week" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(created).toEqual(["p2"]));
  });

  it("has no project picker when there are no projects", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    await screen.findByLabelText("Message");
    expect(screen.queryByTestId("project-picker")).toBeNull();
  });
});

describe("Sidebar chat menu (#166)", () => {
  const setup = () => {
    const f = fakeApi();
    f.projects.push({ id: "p1", name: "Thesis", instructions: "", created_at: 0, updated_at: 0 });
    f.convs.push({ id: "c1", title: "Draft", model: null, project_id: null, created_at: 0, updated_at: 0 });
    return f;
  };

  it("opens from the keyboard, moves with the arrow keys, and Esc returns focus to the trigger", async () => {
    const { api } = setup();
    render(<App api={api} />);
    const trigger = await chatMenu("Draft");
    const items = screen.getAllByRole("menuitem").map((i) => i.textContent);
    expect(items).toEqual(["Move to project", "Rename", "Delete"]);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(trigger));
  });

  it("moves a chat into a project from the submenu, with Undo", async () => {
    const { api, project, convs } = setup();
    render(<App api={api} />);
    await chatMenu("Draft");
    const sub = screen.getByRole("menuitem", { name: "Move to project" });
    sub.focus();
    fireEvent.keyDown(sub, { key: "ArrowRight" });
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "Thesis" }));
    expect(await screen.findByText("Moved to “Thesis”")).toBeTruthy();
    expect(project.moves).toEqual([["c1", "p1"]]);
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(convs[0]?.project_id).toBeNull());
  });

  it("renames inline: Enter saves, and focus goes back to the row", async () => {
    const { api, project } = setup();
    render(<App api={api} />);
    await chatMenuItem("Draft", "Rename");
    const input = (await screen.findByLabelText("Chat name")) as HTMLInputElement;
    await waitFor(() => expect(document.activeElement).toBe(input));
    fireEvent.change(input, { target: { value: "Final report" } });
    fireEvent.submit(input.closest("form")!);
    expect(await screen.findByRole("button", { name: "Final report" })).toBeTruthy();
    expect(project.renames).toEqual([["c1", "Final report"]]);
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Final report" })));
  });

  it("Esc cancels a rename without saving", async () => {
    const { api, project } = setup();
    render(<App api={api} />);
    await chatMenuItem("Draft", "Rename");
    const input = await screen.findByLabelText("Chat name");
    fireEvent.change(input, { target: { value: "Something else" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(await screen.findByRole("button", { name: "Draft" })).toBeTruthy();
    expect(project.renames).toEqual([]);
  });

  it("keeps the typed name and says why when renaming fails, and refuses an empty name", async () => {
    const { api } = setup();
    api.renameConversation = async () => {
      throw { code: "storage", message: "Could not read or write local data." };
    };
    render(<App api={api} />);
    await chatMenuItem("Draft", "Rename");
    const input = (await screen.findByLabelText("Chat name")) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.submit(input.closest("form")!);
    expect(await screen.findByText("A chat name is 1–200 characters.")).toBeTruthy();
    fireEvent.change(input, { target: { value: "Final report" } });
    fireEvent.submit(input.closest("form")!);
    expect(await screen.findByText(/Couldn't rename the chat. Could not read or write local data./)).toBeTruthy();
    expect(input.value).toBe("Final report");
    expect(input.getAttribute("aria-invalid")).toBe("true");
  });

  it("deletes from the menu with Undo, and focus moves to New chat", async () => {
    const { api } = setup();
    render(<App api={api} />);
    await chatMenuItem("Draft", "Delete");
    expect(await screen.findByText("Deleted “Draft”")).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("new-chat")));
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(await screen.findByRole("button", { name: "Draft" })).toBeTruthy();
  });

  it("has no Move to project entry when there are no projects", async () => {
    const { api, convs } = fakeApi();
    convs.push({ id: "c1", title: "Draft", model: null, project_id: null, created_at: 0, updated_at: 0 });
    render(<App api={api} />);
    await chatMenu("Draft");
    expect(screen.getAllByRole("menuitem").map((i) => i.textContent)).toEqual(["Rename", "Delete"]);
  });
});

describe("No silent fallbacks (#141)", () => {
  const fail = () => {
    throw { code: "storage", message: "Could not read or write local data." };
  };

  it("says chats can't start when the assistant's instructions didn't load", async () => {
    const { api } = fakeApi();
    const info = api.appInfo;
    api.appInfo = async () => ({ ...(await info()), agent_error: "the skills folder wasn't found" });
    render(<App api={api} />);
    const banner = await screen.findByTestId("agent-error");
    expect(banner.textContent).toContain("chats can't start");
    expect(banner.textContent).toContain("the skills folder wasn't found");
    expect(within(banner).getByRole("button", { name: "Report a problem" })).toBeTruthy();
  });

  it("shows the list's error, not a guessed list, if reloading after deleting a chat fails", async () => {
    const { api, convs } = fakeApi();
    convs.push({ id: "c1", title: "Gone", model: null, project_id: null, created_at: 0, updated_at: 0 });
    convs.push({ id: "c2", title: "Kept", model: null, project_id: null, created_at: 0, updated_at: 0 });
    render(<App api={api} />);
    await chatMenu("Gone");
    api.listConversations = async () => fail();
    fireEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
    expect(await screen.findByText(/Couldn't load chats/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Kept" })).toBeNull();
  });

  it("keeps the AI notice and says so when dismissing it can't be saved", async () => {
    const { api } = fakeApi();
    api.updateSettings = async () => fail();
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "Got it" }));
    expect(await screen.findByText(/Couldn't save that you've read the notice/)).toBeTruthy();
    expect(screen.getByText(/Demios lets you chat with AI models you choose/)).toBeTruthy();
  });

  it("shows a model-list error with Retry instead of a stale catalog", async () => {
    const { api } = fakeApi();
    const list = api.listModels;
    let broken = true;
    api.listModels = async () => (broken ? fail() : list());
    render(<App api={api} />);
    const banner = await screen.findByTestId("catalog-error");
    expect(banner.textContent).toContain("Couldn't load your models.");
    broken = false;
    fireEvent.click(within(banner).getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.queryByTestId("catalog-error")).toBeNull());
  });

  it("says so when the saved answer can't be loaded after it finishes", async () => {
    const { api, emit } = fakeApi();
    render(<App api={api} />);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "Hello" } });
    fireEvent.keyDown(box, { key: "Enter" });
    await screen.findByRole("button", { name: "Stop answer" });
    api.listMessages = async () => fail();
    act(() => {
      emit({ type: "text_delta", message_id: "id-3", offset: 0, text: "Hi" });
      emit({ type: "done", message_id: "id-3", finish_reason: "stop", usage: { input_tokens: 1, output_tokens: 1 } });
    });
    expect(await screen.findByText(/Couldn't load the saved answer/)).toBeTruthy();
  });

  it("says demios:// links can't open when watching for them fails", async () => {
    const { api } = fakeApi();
    api.watchAddLinks = async () => fail();
    render(<App api={api} />);
    expect(await screen.findByText(/demios:\/\/ links can't open in this window/)).toBeTruthy();
  });

  it("shows an error with Retry when the models on this device can't be loaded", async () => {
    const { api } = fakeApi();
    const watch = api.watchLocalModels;
    let broken = true;
    api.watchLocalModels = async (cb) => (broken ? fail() : watch(cb));
    render(<App api={api} />);
    await openSettings("Models");
    expect(await screen.findByText(/Couldn't load the models on this device/)).toBeTruthy();
    expect(screen.queryByText("No models in the catalogue.")).toBeNull();
    broken = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.queryByText(/Couldn't load the models on this device/)).toBeNull());
  });
});

describe("Settings sections (#174)", () => {
  it("lists General first, marks the open section, and moves with the arrow keys", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    await openSettings("General");
    const nav = screen.getByRole("navigation", { name: "Settings sections" });
    const items = within(nav).getAllByRole("button");
    expect(items.map((b) => b.textContent)).toEqual(["General", "Models", "Tools", "Skills"]);
    expect(items[0]!.getAttribute("aria-current")).toBe("page");
    // Roving focus: only the open section is in the Tab order.
    expect(items.map((b) => b.tabIndex)).toEqual([0, -1, -1, -1]);
    items[0]!.focus();
    fireEvent.keyDown(items[0]!, { key: "ArrowDown" });
    expect(await screen.findByRole("heading", { level: 1, name: "Models" })).toBeTruthy();
    await waitFor(() => expect(document.activeElement?.textContent).toBe("Models"));
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    expect(await screen.findByRole("heading", { level: 1, name: "Skills" })).toBeTruthy();
    fireEvent.keyDown(document.activeElement!, { key: "Home" });
    expect(await screen.findByRole("heading", { level: 1, name: "General" })).toBeTruthy();
  });

  it("opens Tools and Skills on their own pages", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    await openSettings("Tools");
    expect(await screen.findByRole("button", { name: "Add tool server" })).toBeTruthy();
    settingsSection("Skills");
    expect(await screen.findByRole("heading", { level: 1, name: "Skills" })).toBeTruthy();
    // Skills moved out of General.
    settingsSection("General");
    await screen.findByRole("heading", { level: 1, name: "General" });
    expect(screen.queryByRole("heading", { level: 2, name: "Skills" })).toBeNull();
  });

  it("remembers the last page and opens it again from the sidebar and ⌘,", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    await openSettings("Models");
    fireEvent.click(screen.getByRole("tab", { name: "Providers" }));
    settingsSection("Models");
    fireEvent.click(screen.getByTestId("new-chat"));
    await screen.findByLabelText("Message");
    fireEvent.click(screen.getByRole("button", { name: /^Settings/ }));
    expect(await screen.findByRole("heading", { level: 1, name: "Models" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "Providers" }).getAttribute("aria-selected")).toBe("true");
    fireEvent.click(screen.getByTestId("new-chat"));
    await screen.findByLabelText("Message");
    fireEvent.keyDown(window, { key: ",", metaKey: true, ctrlKey: true });
    expect(await screen.findByRole("heading", { level: 1, name: "Models" })).toBeTruthy();
  });

  it("stays in Settings when it is opened while the chat list is still loading", async () => {
    const { api } = fakeApi();
    let release: () => void = () => {};
    const list = api.listConversations;
    api.listConversations = () => new Promise((resolve) => (release = () => void list().then(resolve)));
    render(<App api={api} />);
    await openSettings("Models");
    await act(async () => release());
    expect(screen.getByRole("heading", { level: 1, name: "Models" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "Model servers" })).toBeTruthy();
  });

  it("moves between the Models tabs with the arrow keys, Home and End", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    await openSettings("Models");
    const device = screen.getByRole("tab", { name: "On this device" });
    device.focus();
    fireEvent.keyDown(device, { key: "ArrowRight" });
    const servers = screen.getByRole("tab", { name: "Model servers" });
    expect(servers.getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(servers);
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe("models-tab-servers");
    expect(within(screen.getByRole("tabpanel")).getByRole("button", { name: "Add model server" })).toBeTruthy();
    fireEvent.keyDown(servers, { key: "ArrowRight" });
    const providers = screen.getByRole("tab", { name: "Providers" });
    expect(providers.getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe("models-tab-providers");
    fireEvent.keyDown(providers, { key: "Home" });
    expect(screen.getByRole("tab", { name: "On this device" }).getAttribute("aria-selected")).toBe("true");
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    expect(screen.getByRole("tab", { name: "Providers" }).getAttribute("aria-selected")).toBe("true");
  });

  it("offers a select for the section on narrow windows", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    await openSettings("General");
    const select = screen.getByLabelText("Settings section") as HTMLSelectElement;
    expect(select.value).toBe("general");
    fireEvent.change(select, { target: { value: "tools" } });
    expect(await screen.findByRole("heading", { level: 1, name: "Tools" })).toBeTruthy();
  });

  it("has one Settings entry in the app sidebar", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    const side = await screen.findByRole("navigation", { name: "Chats" });
    expect(within(side).queryByRole("button", { name: "Models" })).toBeNull();
    expect(within(side).getByRole("button", { name: /^Settings/ })).toBeTruthy();
  });
});

describe("System keychain state in Settings (#200)", () => {
  const NO_KEYCHAIN = "No system keychain found. Install GNOME Keyring or KWallet to save API keys.";

  /** A catalog whose keychain failed: every key state unknown, the reason once. */
  function failing(api: Api, keychain: ModelCatalog["keychain"]) {
    const list = api.listModels;
    const broken = async () => {
      const c = await list();
      return {
        ...c,
        keychain,
        providers: c.providers.map((p) => ({ ...p, key: "unknown" as const })),
      };
    };
    api.listModels = broken;
    api.checkKeys = broken;
  }

  for (const [state, message] of [
    ["unavailable", NO_KEYCHAIN],
    ["denied", "Demios wasn't allowed to read your system keychain, so it can't tell which API keys are saved."],
  ] as const) {
    it(`says why keys can't be checked when the keychain is ${state}, never "No key yet"`, async () => {
      const { api } = fakeApi();
      failing(api, { state, message });
      render(<App api={api} />);
      await openSettings("Models");
      fireEvent.click(screen.getByRole("tab", { name: "Providers" }));
      expect(await screen.findByText(message)).toBeTruthy();
      expect(screen.getByText(/Can't tell whether a key is saved/)).toBeTruthy();
      expect(screen.queryByText("No key yet.")).toBeNull();
      expect(screen.queryByRole("button", { name: "Remove key" })).toBeNull();
      // The same message on the Model servers tab, with Retry.
      fireEvent.click(screen.getByRole("tab", { name: "Model servers" }));
      expect(await screen.findByText(message)).toBeTruthy();
      expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
    });
  }

  it("shows the list at once while the keychain is checked, then the keychain's answer", async () => {
    const { api } = fakeApi();
    const list = api.listModels;
    api.listModels = async () => {
      const c = await list();
      return { ...c, keychain: { state: "checking" }, providers: c.providers.map((p) => ({ ...p, key: "unknown" as const })) };
    };
    let answer: () => void = () => {};
    api.checkKeys = () => new Promise((resolve) => (answer = () => void list().then(resolve)));
    render(<App api={api} />);
    await openSettings("Models");
    fireEvent.click(screen.getByRole("tab", { name: "Providers" }));
    expect(await screen.findByText("Checking your system keychain for saved keys…")).toBeTruthy();
    expect(screen.getByLabelText("OpenAI")).toBeTruthy();
    await act(async () => answer());
    expect(await screen.findByText("No key yet.")).toBeTruthy();
    expect(screen.queryByText(/Checking your system keychain/)).toBeNull();
  });

  it("chat says the keychain can't be read instead of \"No model connected\"", async () => {
    const { api } = fakeApi();
    failing(api, { state: "unavailable", message: NO_KEYCHAIN });
    const broken = api.listModels;
    api.appInfo = async () => ({ version: "0.1.0", dev_fake_model: false, data_dir: "/tmp", models_dir: "/tmp/models", updates: "off" });
    api.listModels = api.checkKeys = async () => ({ ...(await broken()), models: [] });
    render(<App api={api} />);
    expect(await screen.findByText(NO_KEYCHAIN)).toBeTruthy();
    expect(screen.queryByText(/No model connected/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Open Providers" }));
    expect(await screen.findByRole("heading", { level: 1, name: "Models" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "Providers" }).getAttribute("aria-selected")).toBe("true");
  });

  it("lists what it found in the keychain and says when it couldn't check the rest", async () => {
    const { api } = fakeApi();
    const summary = api.localDataSummary;
    api.localDataSummary = async () => ({ ...(await summary()), keychain: { entries: [], problem: NO_KEYCHAIN } });
    render(<App api={api} />);
    await openSettings("General");
    fireEvent.click(await screen.findByRole("button", { name: "Delete all local data" }));
    expect(await screen.findByText(new RegExp(`Couldn't check all of them. ${NO_KEYCHAIN}`))).toBeTruthy();
    expect(screen.queryByText(/none saved/)).toBeNull();
  });
});

describe("KeychainNotice (#200)", () => {
  afterEach(() => vi.useRealTimers());

  it("says the keychain is locked or not answering after a while, and Retry asks again", async () => {
    vi.useFakeTimers();
    const { api } = fakeApi();
    const answered = await api.checkKeys();
    const checkKeys = vi.fn(async () => answered);
    api.checkKeys = checkKeys;
    const onCatalog = vi.fn();
    render(
      <ToastProvider>
        <KeychainNotice api={api} status={{ state: "checking" }} onCatalog={onCatalog} />
      </ToastProvider>,
    );
    expect(screen.getByText("Checking your system keychain for saved keys…")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    act(() => vi.advanceTimersByTime(KEYCHAIN_SLOW_MS));
    expect(screen.getByText(/locked or not answering/)).toBeTruthy();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Retry" })));
    expect(checkKeys).toHaveBeenCalledTimes(1);
    expect(onCatalog).toHaveBeenCalledWith(answered);
  });
});
