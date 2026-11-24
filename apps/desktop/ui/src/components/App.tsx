import { PanelLeft, PanelRight } from "lucide-react";
import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { api as realApi, type Api } from "../ipc/commands";
import {
  errorMessage,
  messageText,
  type AppInfo,
  type ChatEvent,
  type Conversation,
  type Id,
  type ModelCatalog,
} from "../ipc/types";
import { chatModel, chatModelReady, describeModel } from "../models";
import { isMac, mod } from "../platform";
import { chatReducer, initialChat } from "../state/chat";
import { ArtifactPane } from "./ArtifactPane";
import { ChatView } from "./ChatView";
import { ModelBadge } from "./ModelBadge";
import { Badge, ICON, IconButton, TooltipProvider } from "./primitives";
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
  const [hidden, setHidden] = useState<Set<Id>>(new Set());
  const [view, setView] = useState<View>({ kind: "chat" });
  const [chat, dispatch] = useReducer(chatReducer, initialChat);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [announce, setAnnounce] = useState("");
  const [sidebarOpen, setSidebarOpen] = useState(() => readPref("dn.sidebar", true));
  const [artifactsOpen, setArtifactsOpen] = useState(() => readPref("dn.artifacts", true));
  const composerRef = useRef<HTMLTextAreaElement>(null);

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

  const open = useCallback(
    async (id: Id | null) => {
      setView({ kind: "chat" });
      setError(null);
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
    [api],
  );

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

  const onEvent = useCallback(
    (event: ChatEvent) => {
      dispatch({ kind: "event", event });
      if (event.type === "done") {
        setAnnounce(event.finish_reason === "cancelled" ? "Answer stopped" : "Answer complete");
        void refreshConversations();
      } else if (event.type === "error") {
        setAnnounce("Answer failed");
        void refreshConversations();
      }
    },
    [refreshConversations],
  );

  const newChat = useCallback(() => {
    // A conversation row is created on the first message, so empty chats don't pile up.
    void open(null);
  }, [open]);

  const send = useCallback(
    async (text: string): Promise<boolean> => {
      setError(null);
      try {
        let id = chat.conversationId;
        if (id === null) {
          const c = await api.createConversation();
          id = c.id;
          dispatch({ kind: "open", conversationId: id, messages: [] });
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
    try {
      const turn = await api.regenerate(id, onEvent);
      dispatch({ kind: "turn_started", conversationId: id, turn });
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api, chat.conversationId, onEvent]);

  const stop = useCallback(async () => {
    if (chat.conversationId !== null && chat.streamingId !== null) {
      await api.stopGeneration(chat.conversationId);
    }
  }, [api, chat.conversationId, chat.streamingId]);

  // Delete acts at once and offers Undo for 10 s; the row is removed for real afterwards.
  const remove = useCallback(
    (c: Conversation) => {
      setHidden((h) => new Set(h).add(c.id));
      if (c.id === chat.conversationId) {
        void api.stopGeneration(c.id);
        const next = conversations.find((x) => x.id !== c.id && !hidden.has(x.id));
        void open(next?.id ?? null);
      }
      toast({
        tone: "info",
        title: `Deleted “${c.title}”`,
        duration: 10_000,
        action: {
          label: "Undo",
          onAction: () =>
            setHidden((h) => {
              const n = new Set(h);
              n.delete(c.id);
              return n;
            }),
        },
        onExpire: () => {
          void api
            .deleteConversation(c.id)
            .then(refreshConversations)
            .catch((e: unknown) => toast({ tone: "error", title: `Couldn't delete the chat. ${errorMessage(e)}` }));
        },
      });
    },
    [api, chat.conversationId, conversations, hidden, open, refreshConversations, toast],
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
        setView({ kind: "settings", section: "models" });
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
  const visible = conversations.filter((c) => !hidden.has(c.id));
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
          conversations={visible}
          state={listState}
          activeId={view.kind === "chat" ? chat.conversationId : null}
          settings={view.kind === "settings" ? view.section : null}
          onNew={newChat}
          onOpen={(id) => void open(id)}
          onDelete={remove}
          onRetry={() => void refreshConversations()}
          onSettings={goSettings}
        />
      )}
      <main className="main">
        {view.kind === "chat" ? (
          <ChatView
            state={chat}
            loading={loadingMessages}
            ready={ready}
            catalog={catalog}
            model={model}
            error={error}
            composerRef={composerRef}
            onSend={send}
            onStop={() => void stop()}
            onRegenerate={() => void regenerate()}
            onConnect={() => goSettings("models")}
          />
        ) : (
          <SettingsView
            api={api}
            info={info}
            section={view.section}
            onSection={goSettings}
            onCatalog={setCatalog}
          />
        )}
      </main>
      {artifactsOpen && <ArtifactPane />}
      <div className="dn-visually-hidden" role="status" aria-live="polite">
        {announce}
      </div>
    </div>
  );
}
