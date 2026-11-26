import { PanelLeft, PanelRight } from "lucide-react";
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { api as realApi, type Api } from "../ipc/commands";
import {
  errorMessage,
  messageText,
  type AppInfo,
  type ApprovalDecision,
  type ApprovalPrompt,
  type ArtifactDetail,
  type ArtifactSummary,
  type ChatEvent,
  type Conversation,
  type SkillInfo,
  type TurnStarted,
  type Id,
  type McpServerStatus,
  type ModelCatalog,
  type Project,
  type ProjectSummary,
  type ReportKind,
  type AddModelLink,
  type Settings,
  type UnknownOutcome,
  type UpdateCheck,
} from "../ipc/types";
import { markArtifact } from "../artifactMarks";
import { onHubDownloadFinished } from "../hubDownloads";
import { chatModel, chatModelReady, describeModel, modelName } from "../models";
import { artifactFrameUrl, isMac, mod } from "../platform";
import { chatReducer, initialChat } from "../state/chat";
import { ArtifactPane, NO_ARTIFACT_TOOLS } from "./ArtifactPane";
import { configureSources } from "./Sources";
import { ChatView, type ArtifactCardInfo } from "./ChatView";
import { ModelBadge } from "./ModelBadge";
import { NewProjectView, ProjectView, type ProjectDraft } from "./ProjectView";
import { Badge, Button, ICON, IconButton, InlineMessage, TooltipProvider } from "./primitives";
import { SettingsView, lastSettingsRoute, type SettingsRoute } from "./SettingsView";
import { Sidebar, type ListState } from "./Sidebar";
import { ToastProvider, useToast } from "./toast";

type View =
  | { kind: "chat" }
  | { kind: "settings"; route: SettingsRoute }
  | { kind: "project"; id: Id | "new" };

export function App({ api = realApi }: { api?: Api }) {
  configureSources(api.messageSources, api.knowledgeStatus);
  return (
    <TooltipProvider>
      <ToastProvider>
        <Shell api={api} />
      </ToastProvider>
    </TooltipProvider>
  );
}

function readPref(key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v === "1";
  } catch {
    return fallback;
  }
}

// Below this width the artifact pane covers the whole window instead of sitting beside the
// chat (the `max-width: 1023px` rule in styles.css). There it opens only when asked or when an
// artifact arrives, never at launch, so Demios doesn't start behind an empty pane.
const ARTIFACT_OVERLAY_QUERY = "(max-width: 1023px)";

function artifactPaneAtLaunch(): boolean {
  const overlays = typeof window.matchMedia === "function" && window.matchMedia(ARTIFACT_OVERLAY_QUERY).matches;
  return !overlays && readPref("dn.artifacts", true);
}

function writePref(key: string, v: boolean) {
  try {
    localStorage.setItem(key, v ? "1" : "0");
  } catch {
    // Preferences are a convenience; ignore storage failures.
  }
}

function Shell({ api }: { api: Api }) {
  const toast = useToast();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  // Chat skills (T-SKL-01): the library and the open chat's choice. A new chat keeps its
  // choice here until its first message creates the conversation.
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [skill, setSkill] = useState<string | null>(null);
  // Which answers of the open chat were written with a skill (message id → label).
  const [answerSkills, setAnswerSkills] = useState<Record<Id, string>>({});
  const [listState, setListState] = useState<ListState>({ kind: "loading" });
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [projectsState, setProjectsState] = useState<ListState>({ kind: "loading" });
  // The project a chat that isn't created yet (no message sent) will belong to.
  const [newChatProject, setNewChatProject] = useState<Id | null>(null);
  // Unsaved project edits survive a look at a chat and back.
  const [projectDrafts, setProjectDrafts] = useState<Record<Id, ProjectDraft>>({});
  const [view, setView] = useState<View>({ kind: "chat" });
  const [chat, dispatch] = useReducer(chatReducer, initialChat);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [announce, setAnnounce] = useState("");
  const [sidebarOpen, setSidebarOpen] = useState(() => readPref("dn.sidebar", true));
  const [artifactsOpen, setArtifactsOpen] = useState(artifactPaneAtLaunch);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const [approvals, setApprovals] = useState<ApprovalPrompt[]>([]);
  const [unknowns, setUnknowns] = useState<UnknownOutcome[]>([]);
  const [servers, setServers] = useState<McpServerStatus[]>([]);
  const [artifacts, setArtifacts] = useState<ArtifactSummary[]>([]);
  const [artifactDetails, setArtifactDetails] = useState<Record<Id, ArtifactDetail>>({});
  const [openArtifact, setOpenArtifact] = useState<Id | null>(null);
  const openIdRef = useRef<Id | null>(null);
  const announceRef = useRef<((msg: string) => void) | null>(null);
  const shownApprovals = useRef<Set<Id>>(new Set());
  openIdRef.current = chat.conversationId;
  shownApprovals.current = new Set(approvals.map((a) => a.request_id));

  const devFake = info?.dev_fake_model ?? false;
  const ready = chatModelReady(catalog, devFake);
  const activeConversation = conversations.find((c) => c.id === chat.conversationId);
  const chatProjectId = chat.conversationId === null ? newChatProject : (activeConversation?.project_id ?? null);
  const chatProject = chatProjectId === null ? undefined : projects.find((p) => p.id === chatProjectId);
  // First-run AI notice (T-STORE-09): shown until dismissed; the dismissal is a setting, so
  // "Delete all local data" brings it back.
  const showAiNotice = catalog !== null && catalog.settings.ai_notice_seen !== true;
  const report = useCallback(
    async (kind: ReportKind, messageId: Id | null) => {
      try {
        await api.reportProblem(kind, messageId);
        toast({ tone: "info", title: "Opened the report form in your browser" });
      } catch (e) {
        toast({ tone: "error", title: errorMessage(e) });
      }
    },
    [api, toast],
  );

  // The opt-in launch check (#95): a newer version shows a notice; nothing downloads.
  const [launchUpdate, setLaunchUpdate] = useState<UpdateCheck | null>(null);
  const [updateNotice, setUpdateNotice] = useState(false);
  const [revealUpdates, setRevealUpdates] = useState(false);
  useEffect(() => {
    if (info?.updates !== "self_update") return;
    let alive = true;
    void api
      .launchUpdateCheck()
      .then((r) => {
        if (!alive || r?.status !== "available") return;
        setLaunchUpdate(r);
        setUpdateNotice(true);
      })
      // Silent: the next launch tries again.
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [api, info?.updates]);

  // The composer's draft survives a switch to Settings (a link, ⌘,) and back.
  const [draft, setDraft] = useState("");

  // demios://models/add links from the OS open the add-model confirmation (T-INF-12).
  const [addLink, setAddLink] = useState<AddModelLink | null>(null);
  const addLinkRef = useRef<AddModelLink | null>(null);
  addLinkRef.current = addLink;
  useEffect(() => {
    let alive = true;
    const show = (l: AddModelLink) => {
      if (!alive) return;
      addLinkRef.current = l;
      setAddLink(l);
      setView({ kind: "settings", route: { section: "models", tab: "device" } });
    };
    void api
      .watchAddLinks(show)
      .then((pending) => {
        if (pending !== null) show(pending);
      })
      .catch((e: unknown) => {
        if (alive) toast({ tone: "error", title: `demios:// links can't open in this window. ${errorMessage(e)}` });
      });
    return () => {
      alive = false;
    };
  }, [api, toast]);
  const closeAddLink = useCallback(() => {
    setAddLink(null);
    void api
      .addLinkDone()
      .catch((e: unknown) =>
        toast({ tone: "error", title: `Couldn't close the link. Restart Demios before opening another one. ${errorMessage(e)}` }),
      );
  }, [api, toast]);

  const openLink = useCallback(
    (href: string) => {
      api.openLink(href).catch((e: unknown) => toast({ tone: "error", title: errorMessage(e) }));
    },
    [api, toast],
  );

  const dismissAiNotice = useCallback(async () => {
    const hide = (settings: Settings) =>
      setCatalog((c) => (c === null ? c : { ...c, settings: { ...settings, ai_notice_seen: true } }));
    try {
      // Start from the stored settings, not this render's copy (another screen may have saved).
      const current = await api.getSettings();
      hide(await api.updateSettings({ ...current, ai_notice_seen: true }));
    } catch (e) {
      // The notice stays: hiding it would pretend the choice was saved (#141).
      toast({ tone: "error", title: `Couldn't save that you've read the notice. ${errorMessage(e)}` });
    }
  }, [api, toast]);

  const refreshConversations = useCallback(async () => {
    try {
      setConversations(await api.listConversations());
      setListState({ kind: "ready" });
    } catch (e) {
      setListState({ kind: "error", message: errorMessage(e) });
    }
  }, [api]);

  const refreshProjects = useCallback(async () => {
    try {
      setProjects(await api.listProjects());
      setProjectsState({ kind: "ready" });
    } catch (e) {
      setProjectsState({ kind: "error", message: errorMessage(e) });
    }
  }, [api]);

  // A failed load is shown above the chat with Retry, not hidden behind a stale catalog
  // that would make the composer say no model is set up (#141).
  const [catalogError, setCatalogError] = useState<string | null>(null);
  // A Hugging Face download from Settings finished, wherever the user is now: pickers and
  // the message box must see the new model.
  useEffect(
    () =>
      onHubDownloadFinished((d) => {
        toast({ tone: "success", title: `${d.label} downloaded` });
        void refreshCatalogRef.current();
      }),
    [toast],
  );
  const refreshCatalogRef = useRef<() => Promise<void>>(async () => {});

  const refreshCatalog = useCallback(async () => {
    try {
      setCatalog(await api.listModels());
      setCatalogError(null);
    } catch (e) {
      setCatalogError(errorMessage(e));
    }
  }, [api]);
  refreshCatalogRef.current = refreshCatalog;

  const refreshUnknowns = useCallback(
    async (id: Id | null) => {
      if (id === null) {
        setUnknowns([]);
        return;
      }
      try {
        const list = await api.listUnknownOutcomes(id);
        setUnknowns(list);
        if (list.length > 0) announceRef.current?.("A tool call needs checking: did it go through?");
      } catch {
        setUnknowns([]);
      }
    },
    [api],
  );

  // Artifacts of the open chat, with every version (cards need to know which message made
  // which version). `focus` opens that artifact and the pane (it was just created or changed).
  const refreshArtifacts = useCallback(
    async (id: Id | null, focus?: Id) => {
      if (id === null) {
        setArtifacts([]);
        setArtifactDetails({});
        setOpenArtifact(null);
        return;
      }
      try {
        // Summaries are cheap; full versions are fetched for every artifact only when a chat
        // opens (the cards need them), and afterwards only for the one that changed.
        const list = await api.listArtifacts(id);
        const ids = focus !== undefined ? [focus] : list.map((a) => a.id);
        const details = await Promise.all(ids.map((aid) => api.getArtifact(id, aid)));
        if (openIdRef.current !== id) return;
        setArtifacts(list);
        setArtifactDetails((all) => {
          const fresh = Object.fromEntries(details.map((d) => [d.id, d]));
          return focus !== undefined ? { ...all, ...fresh } : fresh;
        });
        setOpenArtifact((cur) =>
          focus !== undefined
            ? focus
            : cur !== null && list.some((a) => a.id === cur)
              ? cur
              : (list.at(-1)?.id ?? null),
        );
        if (focus !== undefined) {
          setArtifactsOpen(true);
          writePref("dn.artifacts", true);
        }
      } catch (e) {
        toast({ tone: "error", title: errorMessage(e) });
      }
    },
    [api, toast],
  );

  const artifactCards = useMemo(() => {
    const cards: Record<Id, ArtifactCardInfo[]> = {};
    for (const a of Object.values(artifactDetails)) {
      // The newest version each message made of this artifact.
      const byMessage = new Map<Id, number>();
      for (const v of a.versions) {
        if (v.message_id !== null) byMessage.set(v.message_id, Math.max(v.n, byMessage.get(v.message_id) ?? 0));
      }
      for (const [mid, n] of byMessage) {
        (cards[mid] ??= []).push({ id: a.id, title: a.title, type: a.artifact_type, version: n });
      }
    }
    return cards;
  }, [artifactDetails]);

  const open = useCallback(
    // `keepView`: load the chat without switching to it (startup, where the user may already
    // have opened Settings or a project while the list was loading).
    async (id: Id | null, { keepView = false }: { keepView?: boolean } = {}) => {
      if (!keepView) setView({ kind: "chat" });
      setNewChatProject(null);
      setError(null);
      void refreshUnknowns(id);
      openIdRef.current = id;
      setOpenArtifact(null);
      void refreshArtifacts(id);
      setSkill(null);
      setAnswerSkills({});
      if (id !== null) {
        void api
          .getChatSkill(id)
          .then((s) => {
            if (openIdRef.current === id) setSkill(s);
          })
          .catch((e: unknown) => {
            if (openIdRef.current === id) setError(`Couldn't read this chat's skill. ${errorMessage(e)}`);
          });
        void api
          .listAnswerSkills(id)
          .then((list) => {
            if (openIdRef.current === id) {
              setAnswerSkills(Object.fromEntries(list.map((a) => [a.message_id, a.label])));
            }
          })
          .catch((e: unknown) => {
            if (openIdRef.current === id) setError(`Couldn't read which skills wrote these answers. ${errorMessage(e)}`);
          });
      }
      if (id === null) {
        dispatch({ kind: "open", conversationId: null, messages: [] });
      } else {
        setLoadingMessages(true);
        try {
          dispatch({ kind: "open", conversationId: id, messages: await api.listMessages(id) });
        } catch (e) {
          setError(errorMessage(e));
        } finally {
          setLoadingMessages(false);
        }
      }
      composerRef.current?.focus();
    },
    [api, refreshUnknowns, refreshArtifacts],
  );

  // Tool events arrive on their own channel: approval cards show in their conversation,
  // server status shows on the Models page (failures also as a toast).
  useEffect(() => {
    let alive = true;
    void api
      .watchTools((ev) => {
        if (!alive) return;
        if (ev.type === "requested") {
          setApprovals((a) => [...a.filter((x) => x.request_id !== ev.prompt.request_id), ev.prompt]);
          announceRef.current?.(`Approval needed: ${ev.prompt.title}`);
        } else if (ev.type === "resolved") {
          if (ev.expired && shownApprovals.current.has(ev.request_id)) {
            toast({ tone: "warning", title: "An approval request expired, so the tool didn't run" });
          }
          setApprovals((a) => a.filter((x) => x.request_id !== ev.request_id));
        } else if (ev.type === "mcp_server_removed") {
          setServers((list) => list.filter((x) => x.server_id !== ev.server_id));
        } else {
          setServers((list) => [...list.filter((x) => x.server_id !== ev.status.server_id), ev.status]);
          if (ev.status.state === "failed") {
            toast({ tone: "error", title: `${ev.status.name} couldn't connect. ${ev.status.message ?? ""}`.trim() });
          }
        }
      })
      .then((snap) => {
        if (!alive) return;
        setApprovals(snap.approvals);
        setServers(snap.servers);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [api, toast]);

  // The skill library, read again whenever the chat comes back into view (Settings ›
  // Skills can reload it). An empty library hides the picker; a failed read says so.
  useEffect(() => {
    if (view.kind !== "chat") return;
    let alive = true;
    api
      .listSkills()
      .then((s) => alive && setSkills(s))
      .catch((e: unknown) => alive && setError(`Couldn't load the skills. ${errorMessage(e)}`));
    return () => {
      alive = false;
    };
  }, [api, view.kind]);

  useEffect(() => {
    void (async () => {
      try {
        setInfo(await api.appInfo());
      } catch (e) {
        setError(errorMessage(e));
      }
      await refreshCatalog();
      void refreshProjects();
      try {
        const list = await api.listConversations();
        setConversations(list);
        setListState({ kind: "ready" });
        await open(list[0]?.id ?? null, { keepView: true });
        // A link that started the app wins over opening the last chat.
        if (addLinkRef.current !== null) setView({ kind: "settings", route: { section: "models", tab: "device" } });
      } catch (e) {
        setListState({ kind: "error", message: errorMessage(e) });
      }
    })();
  }, [api, open, refreshCatalog, refreshProjects]);

  const say = useCallback((msg: string) => {
    // Clear first so the same words are announced again.
    setAnnounce("");
    window.setTimeout(() => setAnnounce(msg), 50);
  }, []);
  announceRef.current = say;

  const onEvent = useCallback(
    (event: ChatEvent) => {
      if (event.type === "artifact_changed") {
        if (event.conversation_id === openIdRef.current) {
          void refreshArtifacts(event.conversation_id, event.artifact_id);
          announceRef.current?.(`Artifact ${event.title}, version ${event.version}`);
        }
        return;
      }
      dispatch({ kind: "event", event });
      if (event.type === "done" || event.type === "error") {
        const msg =
          event.type === "error"
            ? "Answer failed"
            : event.finish_reason === "cancelled"
              ? "Answer stopped"
              : "Answer complete";
        // Clear first so the same words are announced again for the next answer.
        setAnnounce("");
        window.setTimeout(() => setAnnounce(msg), 50);
        void refreshConversations();
        // The run's tool calls and results are stored now; show them as saved.
        const id = openIdRef.current;
        if (id !== null) {
          void api
            .listMessages(id)
            .then((messages) => {
              if (openIdRef.current === id && messages.some((m) => m.id === event.message_id)) {
                dispatch({ kind: "open", conversationId: id, messages });
              }
            })
            .catch((e: unknown) => {
              if (openIdRef.current === id) {
                setError(`Couldn't load the saved answer, so what you see may be incomplete. Open the chat again. ${errorMessage(e)}`);
              }
            });
          void refreshUnknowns(id);
        }
      }
    },
    [api, refreshConversations, refreshUnknowns, refreshArtifacts, toast],
  );

  const decide = useCallback(
    async (requestId: Id, d: ApprovalDecision) => {
      try {
        await api.respondApproval(requestId, d);
      } catch (e) {
        toast({ tone: "error", title: errorMessage(e) });
      }
      setApprovals((a) => a.filter((x) => x.request_id !== requestId));
    },
    [api, toast],
  );

  const resolveUnknown = useCallback(
    async (key: string, happened: boolean) => {
      try {
        await api.resolveUnknownOutcome(key, happened);
        toast({ tone: "success", title: happened ? "Marked as done" : "Marked as not done; it can run again" });
      } catch (e) {
        toast({ tone: "error", title: errorMessage(e) });
      }
      void refreshUnknowns(openIdRef.current);
    },
    [api, refreshUnknowns, toast],
  );

  const newChat = useCallback(() => {
    // A conversation row is created on the first message, so empty chats don't pile up.
    void open(null);
  }, [open]);

  const noteAnswerSkill = useCallback((turn: TurnStarted) => {
    const label = turn.skill;
    if (label !== null) setAnswerSkills((m) => ({ ...m, [turn.assistant_message.id]: label }));
  }, []);

  const send = useCallback(
    async (text: string): Promise<boolean> => {
      setError(null);
      dispatch({ kind: "sending" });
      try {
        let id = chat.conversationId;
        if (id === null) {
          const c = await api.createConversation(newChatProject);
          id = c.id;
          if (skill !== null) await api.setChatSkill(id, skill);
          dispatch({ kind: "open", conversationId: id, messages: [] });
          dispatch({ kind: "sending" });
        }
        const turn = await api.sendMessage(id, text, onEvent);
        dispatch({ kind: "turn_started", conversationId: id, turn });
        noteAnswerSkill(turn);
        void refreshConversations();
        return true;
      } catch (e) {
        dispatch({ kind: "turn_failed" });
        setError(errorMessage(e));
        return false;
      }
    },
    [api, chat.conversationId, newChatProject, onEvent, refreshConversations, skill, noteAnswerSkill],
  );

  const chooseSkill = useCallback(
    async (name: string | null) => {
      const id = chat.conversationId;
      const before = skill;
      setSkill(name);
      if (id === null) return;
      try {
        await api.setChatSkill(id, name);
      } catch (e) {
        if (openIdRef.current === id) setSkill(before);
        setError(errorMessage(e));
      }
    },
    [api, chat.conversationId, skill],
  );

  const regenerate = useCallback(async () => {
    const id = chat.conversationId;
    if (id === null) return;
    setError(null);
    dispatch({ kind: "sending" });
    try {
      const turn = await api.regenerate(id, onEvent);
      dispatch({ kind: "turn_started", conversationId: id, turn });
      noteAnswerSkill(turn);
    } catch (e) {
      dispatch({ kind: "turn_failed" });
      setError(errorMessage(e));
    }
  }, [api, chat.conversationId, onEvent, noteAnswerSkill]);

  // Stop by conversation, so ⌘. also works between Send and the reply that names the
  // assistant message.
  const stop = useCallback(async () => {
    if (chat.conversationId !== null) await api.stopGeneration(chat.conversationId);
  }, [api, chat.conversationId]);

  // Delete acts at once (soft delete in the store) and offers Undo for 10 s; the store
  // purges deleted chats on the next start.
  const remove = useCallback(
    async (c: Conversation) => {
      try {
        await api.deleteConversation(c.id);
      } catch (e) {
        toast({ tone: "error", title: `Couldn't delete the chat. ${errorMessage(e)}` });
        return;
      }
      // A failed reload shows the list's error with Retry; no guessed list stands in (#141).
      let next: Id | null = null;
      try {
        const list = await api.listConversations();
        setConversations(list);
        setListState({ kind: "ready" });
        next = list[0]?.id ?? null;
      } catch (e) {
        setListState({ kind: "error", message: errorMessage(e) });
      }
      if (c.id === chat.conversationId) void open(next);
      toast({
        tone: "info",
        title: `Deleted “${c.title}”`,
        duration: 10_000,
        action: {
          label: "Undo",
          onAction: () => {
            void api
              .restoreConversation(c.id)
              .then(refreshConversations)
              .catch((e: unknown) => toast({ tone: "error", title: `Couldn't restore the chat. ${errorMessage(e)}` }));
          },
        },
      });
    },
    [api, chat.conversationId, open, refreshConversations, toast],
  );

  /** A new chat in `projectId`; like any new chat, it is created on the first message. */
  const newChatIn = useCallback(
    (projectId: Id) => {
      void open(null);
      setNewChatProject(projectId);
    },
    [open],
  );

  // Move the open chat into a project or out of one (#136). A chat not created yet just
  // remembers where it will start. Undo moves it back.
  const moveChat = useCallback(
    async (projectId: Id | null, conversationId: Id | null = chat.conversationId) => {
      const id = conversationId;
      if (id === null) {
        setNewChatProject(projectId);
        return;
      }
      const nameOf = (pid: Id | null) => projects.find((x) => x.id === pid)?.name ?? "";
      const move = async (to: Id | null) => {
        const before = await api.moveConversation(id, to);
        await Promise.all([refreshConversations(), refreshProjects()]);
        return before;
      };
      let before: Id | null;
      try {
        before = await move(projectId);
      } catch (e) {
        toast({ tone: "error", title: `Couldn't move the chat. ${errorMessage(e)}` });
        return;
      }
      toast({
        tone: "info",
        title: projectId === null ? `Removed from “${nameOf(before)}”` : `Moved to “${nameOf(projectId)}”`,
        duration: 10_000,
        action: {
          label: "Undo",
          onAction: () => {
            void move(before).catch((e: unknown) =>
              toast({ tone: "error", title: `Couldn't move the chat back. ${errorMessage(e)}` }),
            );
          },
        },
      });
    },
    [api, chat.conversationId, projects, refreshConversations, refreshProjects, toast],
  );

  // Rename from the sidebar menu (#166). Rejects so the row keeps the typed text and shows
  // why; nothing else changes on failure.
  const renameChat = useCallback(
    async (c: Conversation, title: string) => {
      await api.renameConversation(c.id, title);
      await refreshConversations();
    },
    [api, refreshConversations],
  );

  const projectCreated = useCallback(
    (p: Project) => {
      void refreshProjects();
      setView({ kind: "project", id: p.id });
    },
    [refreshProjects],
  );

  const projectSaved = useCallback((p: Project) => {
    setProjects((list) => list.map((x) => (x.id === p.id ? { ...x, ...p } : x)));
  }, []);

  // Delete acts at once (soft delete of the project, its chats and files) and offers Undo
  // for 10 s, like chats; the store purges them on the next start.
  const removeProject = useCallback(
    async (p: ProjectSummary) => {
      let chats: number;
      try {
        chats = await api.deleteProject(p.id);
      } catch (e) {
        toast({ tone: "error", title: `Couldn't delete the project. ${errorMessage(e)}` });
        return;
      }
      setProjectDrafts(({ [p.id]: _gone, ...rest }) => rest);
      void refreshProjects();
      // A failed reload shows the list's error with Retry; no guessed list stands in for it.
      let next: Id | null = null;
      try {
        const list = await api.listConversations();
        setConversations(list);
        setListState({ kind: "ready" });
        next = list[0]?.id ?? null;
      } catch (e) {
        setListState({ kind: "error", message: errorMessage(e) });
      }
      const openGone = chatProjectId === p.id;
      if (openGone || view.kind === "project") void open(openGone ? next : chat.conversationId);
      toast({
        tone: "info",
        title:
          chats === 0
            ? `Deleted “${p.name}”`
            : `Deleted “${p.name}” and its ${chats === 1 ? "chat" : `${chats} chats`}`,
        duration: 10_000,
        action: {
          label: "Undo",
          onAction: () => {
            void api
              .restoreProject(p.id)
              .then(() => Promise.all([refreshProjects(), refreshConversations()]))
              .catch((e: unknown) =>
                toast({ tone: "error", title: `Couldn't restore the project. ${errorMessage(e)}` }),
              );
          },
        },
      });
    },
    [api, chat.conversationId, chatProjectId, open, refreshConversations, refreshProjects, toast, view.kind],
  );

  const copyLastAnswer = useCallback(() => {
    const last = [...chat.messages].reverse().find((m) => m.role === "assistant");
    if (last === undefined) return;
    void navigator.clipboard
      ?.writeText(messageText(last))
      .then(() => toast({ tone: "success", title: "Answer copied" }));
  }, [chat.messages, toast]);

  const toggleSidebar = useCallback(() => {
    setSidebarOpen((v) => {
      writePref("dn.sidebar", !v);
      return !v;
    });
  }, []);

  const frameUrl = useMemo(() => artifactFrameUrl(), []);

  const openArtifactById = useCallback((aid: Id) => {
    setOpenArtifact(aid);
    setArtifactsOpen(true);
    writePref("dn.artifacts", true);
  }, []);

  const restoreArtifact = useCallback(
    async (aid: Id, n: number) => {
      const id = openIdRef.current;
      if (id === null) return;
      try {
        const d = await api.rollbackArtifact(id, aid, n);
        setArtifactDetails((all) => ({ ...all, [d.id]: d }));
        setArtifacts((list) => list.map((a) => (a.id === d.id ? { ...a, latest_n: d.versions.at(-1)?.n ?? a.latest_n } : a)));
        toast({ tone: "success", title: `Restored v${n} as v${d.versions.at(-1)?.n ?? ""}` });
      } catch (e) {
        toast({ tone: "error", title: errorMessage(e) });
      }
    },
    [api, toast],
  );

  const toggleArtifacts = useCallback(() => {
    setArtifactsOpen((v) => {
      writePref("dn.artifacts", !v);
      return !v;
    });
  }, []);

  // Global shortcuts (design-system §7). ⌘K search arrives with the command palette.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const m = isMac ? e.metaKey : e.ctrlKey;
      if (!m) return;
      const k = e.key.toLowerCase();
      if (k === "n" && !e.shiftKey) {
        e.preventDefault();
        newChat();
      } else if (k === ",") {
        e.preventDefault();
        setView({ kind: "settings", route: lastSettingsRoute() });
      } else if (k === ".") {
        e.preventDefault();
        void stop();
      } else if (k === "\\") {
        e.preventDefault();
        toggleSidebar();
      } else if (k === "c" && e.shiftKey) {
        e.preventDefault();
        copyLastAnswer();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [copyLastAnswer, newChat, stop, toggleSidebar]);

  // Opens a Settings page; with no route, the one the user had open last (#174).
  const goSettings = (route: SettingsRoute = lastSettingsRoute()) => setView({ kind: "settings", route });
  // Artifacts belong to the open chat. Settings and project pages get the full width; the
  // pane comes back as it was when a chat is shown again.
  const showArtifacts = artifactsOpen && view.kind === "chat";
  const otherApprovals = approvals.filter(
    (a) => a.scope.startsWith("conv:") && (view.kind !== "chat" || a.scope !== `conv:${chat.conversationId ?? ""}`),
  );
  const model = chatModel(catalog, devFake);
  const openProject =
    view.kind === "project" && view.id !== "new" ? projects.find((p) => p.id === view.id) : undefined;
  const chatTitle = activeConversation?.title ?? "New chat";
  const title =
    view.kind === "settings"
      ? "Settings"
      : view.kind === "project"
        ? (openProject?.name ?? "New project")
        : chatProject !== undefined
          ? `${chatProject.name} / ${chatTitle}`
          : chatTitle;

  return (
    <div
      className={`app${sidebarOpen ? "" : " sidebar-collapsed"}${showArtifacts ? " artifacts-open" : ""}${isMac ? " os-mac" : ""}`}
    >
      <header className="titlebar" data-tauri-drag-region>
        <IconButton
          label={sidebarOpen ? "Hide sidebar" : "Show sidebar"}
          shortcut={`${mod}\\`}
          onClick={toggleSidebar}
        >
          <PanelLeft {...ICON} aria-hidden="true" />
        </IconButton>
        <div className="titlebar-title" data-tauri-drag-region>
          {title}
        </div>
        <div className="titlebar-end">
          {view.kind === "chat" && <ModelBadge info={describeModel(model, catalog)} />}
          {devFake && model === "local:dev-fake" && <Badge tone="warning">Dev build</Badge>}
          {view.kind === "chat" && (
            <IconButton
              label={artifactsOpen ? "Hide artifacts" : "Show artifacts"}
              onClick={toggleArtifacts}
              aria-pressed={artifactsOpen}
            >
              <PanelRight {...ICON} aria-hidden="true" />
            </IconButton>
          )}
        </div>
      </header>
      {sidebarOpen && (
        <Sidebar
          conversations={conversations}
          state={listState}
          activeId={view.kind === "chat" ? chat.conversationId : null}
          projects={projects}
          projectsState={projectsState}
          activeProject={view.kind === "project" ? view.id : null}
          onNewProject={() => setView({ kind: "project", id: "new" })}
          onOpenProject={(id) => setView({ kind: "project", id })}
          onRetryProjects={() => void refreshProjects()}
          settingsOpen={view.kind === "settings"}
          onNew={newChat}
          onOpen={(id) => void open(id)}
          onDelete={(c) => void remove(c)}
          onMove={(c, pid) => moveChat(pid, c.id)}
          onRename={renameChat}
          onRetry={() => void refreshConversations()}
          onSettings={goSettings}
        />
      )}
      <main className="main">
        {info?.agent_error != null && (
          <div className="approval-elsewhere" data-testid="agent-error">
            <InlineMessage
              tone="error"
              action={
                <Button size="sm" onClick={() => void report("problem", null)}>
                  Report a problem
                </Button>
              }
            >
              Demios couldn't load its assistant instructions, so chats can't start. Reinstall Demios, or report a
              problem. ({info.agent_error})
            </InlineMessage>
          </div>
        )}
        {/* Over the chat only: Settings › Models shows its own error with Retry. */}
        {catalogError !== null && view.kind === "chat" && (
          <div className="approval-elsewhere" data-testid="catalog-error">
            <InlineMessage
              tone="error"
              action={
                <Button size="sm" onClick={() => void refreshCatalog()}>
                  Retry
                </Button>
              }
            >
              Couldn't load your models. {catalogError}
            </InlineMessage>
          </div>
        )}
        {otherApprovals.length > 0 && (
          <div className="approval-elsewhere" role="status">
            <InlineMessage
              tone="warning"
              action={
                <Button size="sm" onClick={() => void open(otherApprovals[0]?.scope.slice(5) ?? null)}>
                  Open chat
                </Button>
              }
            >
              Another chat is waiting for your approval.
            </InlineMessage>
          </div>
        )}
        {updateNotice && launchUpdate?.status === "available" && (
          <div className="approval-elsewhere" data-testid="update-notice">
            <InlineMessage
              tone="info"
              action={
                <span className="update-offer-actions">
                  <Button
                    size="sm"
                    onClick={() => {
                      setUpdateNotice(false);
                      setRevealUpdates(true);
                      setView({ kind: "settings", route: { section: "general" } });
                    }}
                  >
                    View update
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => setUpdateNotice(false)}>
                    Dismiss
                  </Button>
                </span>
              }
            >
              Demios {launchUpdate.version} is available.
            </InlineMessage>
          </div>
        )}
        {view.kind === "chat" ? (
          <ChatView
            state={chat}
            loading={loadingMessages}
            ready={ready}
            catalog={catalog}
            title={activeConversation?.title ?? "New chat"}
            model={model}
            error={error}
            composerRef={composerRef}
            onSend={send}
            onStop={() => void stop()}
            onRegenerate={() => void regenerate()}
            onConnect={() => goSettings({ section: "models", tab: "device" })}
            approvals={approvals.filter((a) => a.scope === `conv:${chat.conversationId ?? ""}`)}
            onDecide={decide}
            unknowns={unknowns}
            onResolveUnknown={resolveUnknown}
            artifactCards={artifactCards}
            onOpenArtifact={openArtifactById}
            onReportAnswer={(m) => void report("answer", m)}
            draft={draft}
            onDraft={setDraft}
            onOpenLink={openLink}
            aiNotice={showAiNotice}
            onDismissAiNotice={() => void dismissAiNotice()}
            skills={skills}
            skill={skill}
            answerSkills={answerSkills}
            onSkill={(n) => void chooseSkill(n)}
            projects={projects}
            projectId={chatProjectId}
            onProject={(pid) => void moveChat(pid)}
          />
        ) : view.kind === "project" ? (
          view.id === "new" ? (
            <NewProjectView api={api} onCreated={projectCreated} />
          ) : openProject === undefined ? (
            <section className="settings" aria-label="Project">
              <div className="settings-column">
                {projectsState.kind === "error" ? (
                  <InlineMessage
                    tone="error"
                    action={
                      <Button size="sm" onClick={() => void refreshProjects()}>
                        Retry
                      </Button>
                    }
                  >
                    Couldn't load the project. {projectsState.message}
                  </InlineMessage>
                ) : projectsState.kind === "loading" ? (
                  <p className="dn-help" role="status">
                    Loading the project…
                  </p>
                ) : (
                  <InlineMessage tone="info">This project was deleted.</InlineMessage>
                )}
              </div>
            </section>
          ) : (
            <ProjectView
              api={api}
              project={openProject}
              chats={conversations.filter((c) => c.project_id === openProject.id)}
              draft={projectDrafts[openProject.id] ?? null}
              onDraft={(d) =>
                setProjectDrafts(({ [openProject.id]: _old, ...rest }) =>
                  d === null ? rest : { ...rest, [openProject.id]: d },
                )
              }
              onSaved={projectSaved}
              onNewChat={() => newChatIn(openProject.id)}
              onOpenChat={(id) => void open(id)}
              onDelete={() => void removeProject(openProject)}
              onFilesChanged={() => void refreshProjects()}
            />
          )
        ) : (
          <SettingsView
            api={api}
            info={info}
            route={view.route}
            onRoute={(route) => setView({ kind: "settings", route })}
            onCatalog={setCatalog}
            servers={servers}
            addLink={addLink}
            onAddLinkClosed={closeAddLink}
            launchUpdate={launchUpdate}
            revealUpdates={revealUpdates}
          />
        )}
      </main>
      {showArtifacts && (
        <ArtifactPane
          artifacts={artifacts}
          detail={openArtifact === null ? null : (artifactDetails[openArtifact] ?? null)}
          frameUrl={frameUrl}
          onOpen={openArtifactById}
          onRestore={(aid, n) => void restoreArtifact(aid, n)}
          onFix={(prompt) => void send(prompt)}
          modelName={(ref) => modelName(ref, catalog)}
          mark={(type, v) => markArtifact(type, v.content, v.model, v.created_at)}
          onClose={toggleArtifacts}
          notice={catalog?.chat_model_tools === false ? NO_ARTIFACT_TOOLS : null}
        />
      )}
      <div className="dn-visually-hidden" role="status" aria-live="polite">
        {announce}
      </div>
    </div>
  );
}
