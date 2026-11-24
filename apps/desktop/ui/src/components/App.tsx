import { useCallback, useEffect, useReducer, useState } from "react";
import { api as realApi, type Api } from "../ipc/commands";
import { errorMessage, type AppInfo, type ChatEvent, type Conversation, type Id } from "../ipc/types";
import { chatReducer, initialChat } from "../state/chat";
import { ArtifactPane } from "./ArtifactPane";
import { ChatView } from "./ChatView";
import { SettingsView } from "./SettingsView";
import { Sidebar } from "./Sidebar";

type View = "chat" | "settings";

export function App({ api = realApi }: { api?: Api }) {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [view, setView] = useState<View>("chat");
  const [chat, dispatch] = useReducer(chatReducer, initialChat);
  const [error, setError] = useState<string | null>(null);

  const refreshConversations = useCallback(async () => {
    setConversations(await api.listConversations());
  }, [api]);

  const open = useCallback(
    async (id: Id | null) => {
      setView("chat");
      setError(null);
      const messages = id === null ? [] : await api.listMessages(id);
      dispatch({ kind: "open", conversationId: id, messages });
    },
    [api],
  );

  useEffect(() => {
    void (async () => {
      try {
        setInfo(await api.appInfo());
        const list = await api.listConversations();
        setConversations(list);
        await open(list[0]?.id ?? null);
      } catch (e) {
        setError(errorMessage(e));
      }
    })();
  }, [api, open]);

  const onEvent = useCallback(
    (event: ChatEvent) => {
      dispatch({ kind: "event", event });
      if (event.type === "done" || event.type === "error") void refreshConversations();
    },
    [refreshConversations],
  );

  const newChat = useCallback(async () => {
    try {
      const c = await api.createConversation();
      await refreshConversations();
      await open(c.id);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api, open, refreshConversations]);

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
        await refreshConversations();
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
    if (chat.conversationId !== null) await api.stopGeneration(chat.conversationId);
  }, [api, chat.conversationId]);

  const remove = useCallback(
    async (id: Id) => {
      try {
        await api.deleteConversation(id);
        const list = await api.listConversations();
        setConversations(list);
        if (id === chat.conversationId) await open(list[0]?.id ?? null);
      } catch (e) {
        setError(errorMessage(e));
      }
    },
    [api, chat.conversationId, open],
  );

  return (
    <div className="app">
      <Sidebar
        conversations={conversations}
        activeId={view === "chat" ? chat.conversationId : null}
        settingsOpen={view === "settings"}
        onNew={() => void newChat()}
        onOpen={(id) => void open(id)}
        onDelete={(id) => void remove(id)}
        onSettings={() => setView("settings")}
        devFake={info?.dev_fake_model ?? false}
      />
      <main className="main">
        {view === "chat" ? (
          <ChatView
            state={chat}
            title={conversations.find((c) => c.id === chat.conversationId)?.title ?? "New chat"}
            error={error}
            onSend={send}
            onStop={() => void stop()}
            onRegenerate={() => void regenerate()}
            onOpenSettings={() => setView("settings")}
          />
        ) : (
          <SettingsView api={api} info={info} onClose={() => setView("chat")} />
        )}
      </main>
      <ArtifactPane />
    </div>
  );
}
