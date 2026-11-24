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
  type Id,
  type McpServerStatus,
  type ModelCatalog,
  type UnknownOutcome,
} from "../ipc/types";
import { chatModel, chatModelReady, describeModel, modelName } from "../models";
import { artifactFrameUrl, isMac, mod } from "../platform";
import { chatReducer, initialChat } from "../state/chat";
import { ArtifactPane } from "./ArtifactPane";
import { ChatView, type ArtifactCardInfo } from "./ChatView";
import { ModelBadge } from "./ModelBadge";
import { Badge, Button, ICON, IconButton, InlineMessage, TooltipProvider } from "./primitives";
import { SettingsView, type SettingsSection } from "./SettingsView";
import { Sidebar, type ListState } from "./Sidebar";
import { ToastProvider, useToast } from "./toast";

type View = { kind: "chat" } | { kind: "settings"; section: SettingsSection };


export function App({ api = realApi }: { api?: Api }) {
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
  const [listState, setListState] = useState<ListState>({ kind: "loading" });
  const [view, setView] = useState<View>({ kind: "chat" });
  const [chat, dispatch] = useReducer(chatReducer, initialChat);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [announce, setAnnounce] = useState("");
  const [sidebarOpen, setSidebarOpen] = useState(() => readPref("dn.sidebar", true));
  const [artifactsOpen, setArtifactsOpen] = useState(() => readPref("dn.artifacts", true));
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

  const refreshConversations = useCallback(async () => {
    try {
      setConversations(await api.listConversations());
      setListState({ kind: "ready" });
    } catch (e) {
      setListState({ kind: "error", message: errorMessage(e) });
    }
  }, [api]);

  const refreshCatalog = useCallback(async () => {
    try {
      setCatalog(await api.listModels());
    } catch {
      // The settings screen shows its own error with Retry.
    }
  }, [api]);

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
        const list = await api.listArtifacts(id);
        const details = await Promise.all(list.map((a) => api.getArtifact(id, a.id)));
        if (openIdRef.current !== id) return;
        setArtifacts(list);
        setArtifactDetails(Object.fromEntries(details.map((d) => [d.id, d])));
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
    async (id: Id | null) => {
      setView({ kind: "chat" });
      setError(null);
      void refreshUnknowns(id);
      openIdRef.current = id;
      setOpenArtifact(null);
      void refreshArtifacts(id);
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

  useEffect(() => {
    void (async () => {
      try {
        setInfo(await api.appInfo());
      } catch (e) {
        setError(errorMessage(e));
      }
      await refreshCatalog();
      try {
        const list = await api.listConversations();
        setConversations(list);
        setListState({ kind: "ready" });
        await open(list[0]?.id ?? null);
      } catch (e) {
        setListState({ kind: "error", message: errorMessage(e) });
      }
    })();
  }, [api, open, refreshCatalog]);

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
            .catch(() => {});
          void refreshUnknowns(id);
        }
      }
    },
    [api, refreshConversations, refreshUnknowns, refreshArtifacts],
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

  const send = useCallback(
    async (text: string): Promise<boolean> => {
      setError(null);
      dispatch({ kind: "sending" });
      try {
        let id = chat.conversationId;
        if (id === null) {
          const c = await api.createConversation();
          id = c.id;
          dispatch({ kind: "open", conversationId: id, messages: [] });
          dispatch({ kind: "sending" });
        }
        const turn = await api.sendMessage(id, text, onEvent);
        dispatch({ kind: "turn_started", conversationId: id, turn });
        void refreshConversations();
        return true;
      } catch (e) {
        dispatch({ kind: "turn_failed" });
        setError(errorMessage(e));
        return false;
      }
    },
    [api, chat.conversationId, onEvent, refreshConversations],
  );

  const regenerate = useCallback(async () => {
    const id = chat.conversationId;
    if (id === null) return;
    setError(null);
    dispatch({ kind: "sending" });
    try {
      const turn = await api.regenerate(id, onEvent);
      dispatch({ kind: "turn_started", conversationId: id, turn });
    } catch (e) {
      dispatch({ kind: "turn_failed" });
      setError(errorMessage(e));
    }
  }, [api, chat.conversationId, onEvent]);

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
      const list = await api.listConversations().catch(() => conversations.filter((x) => x.id !== c.id));
      setConversations(list);
      if (c.id === chat.conversationId) void open(list[0]?.id ?? null);
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
    [api, chat.conversationId, conversations, open, refreshConversations, toast],
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
        setArtifacts((list) => list.map((a) => (a.id === d.id ? { ...a, latest_n: d.versions.length } : a)));
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
        setView({ kind: "settings", section: "general" });
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

  const goSettings = (section: SettingsSection) => setView({ kind: "settings", section });
  const otherApprovals = approvals.filter(
    (a) => a.scope.startsWith("conv:") && (view.kind !== "chat" || a.scope !== `conv:${chat.conversationId ?? ""}`),
  );
  const model = chatModel(catalog, devFake);
  const title =
    view.kind === "settings" ? "Settings" : (activeConversation?.title ?? "New chat");

  return (
    <div
      className={`app${sidebarOpen ? "" : " sidebar-collapsed"}${artifactsOpen ? " artifacts-open" : ""}${isMac ? " os-mac" : ""}`}
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
          <IconButton
            label={artifactsOpen ? "Hide artifacts" : "Show artifacts"}
            onClick={toggleArtifacts}
            aria-pressed={artifactsOpen}
          >
            <PanelRight {...ICON} aria-hidden="true" />
          </IconButton>
        </div>
      </header>
      {sidebarOpen && (
        <Sidebar
          conversations={conversations}
          state={listState}
          activeId={view.kind === "chat" ? chat.conversationId : null}
          settings={view.kind === "settings" ? view.section : null}
          onNew={newChat}
          onOpen={(id) => void open(id)}
          onDelete={(c) => void remove(c)}
          onRetry={() => void refreshConversations()}
          onSettings={goSettings}
        />
      )}
      <main className="main">
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
            onConnect={() => goSettings("models")}
            approvals={approvals.filter((a) => a.scope === `conv:${chat.conversationId ?? ""}`)}
            onDecide={decide}
            unknowns={unknowns}
            onResolveUnknown={resolveUnknown}
            artifactCards={artifactCards}
            onOpenArtifact={openArtifactById}
          />
        ) : (
          <SettingsView
            api={api}
            info={info}
            section={view.section}
            onSection={goSettings}
            onCatalog={setCatalog}
            servers={servers}
          />
        )}
      </main>
      {artifactsOpen && (
        <ArtifactPane
          artifacts={artifacts}
          detail={openArtifact === null ? null : (artifactDetails[openArtifact] ?? null)}
          frameUrl={frameUrl}
          onOpen={openArtifactById}
          onRestore={(aid, n) => void restoreArtifact(aid, n)}
          onFix={(prompt) => void send(prompt)}
          modelName={(ref) => modelName(ref, catalog)}
          onClose={toggleArtifacts}
        />
      )}
      <div className="dn-visually-hidden" role="status" aria-live="polite">
        {announce}
      </div>
    </div>
  );
}
