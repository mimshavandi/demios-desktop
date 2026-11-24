import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { messageText, type StoredMessage } from "../ipc/types";
import { canRegenerate, type ChatState } from "../state/chat";
import { Markdown } from "./Markdown";

interface Props {
  state: ChatState;
  title: string;
  error: string | null;
  onSend: (text: string) => Promise<boolean>;
  onStop: () => void;
  onRegenerate: () => void;
  onOpenSettings: () => void;
}

export function ChatView(p: Props) {
  const [draft, setDraft] = useState("");
  const listRef = useRef<HTMLDivElement>(null);
  const streaming = p.state.streamingId !== null;
  const last = p.state.messages[p.state.messages.length - 1];

  // Follow the stream while the user is near the bottom.
  useEffect(() => {
    const el = listRef.current;
    if (el === null) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 160) el.scrollTop = el.scrollHeight;
  }, [p.state.messages]);

  const submit = async () => {
    const text = draft.trim();
    if (text === "" || streaming) return;
    setDraft("");
    const ok = await p.onSend(text);
    if (!ok) setDraft(text);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void submit();
    }
  };

  return (
    <section className="chat" aria-label="Chat">
      <header className="chat-head">
        <h1>{p.title}</h1>
      </header>
      <div className="messages" ref={listRef} aria-live="polite">
        {p.state.messages.length === 0 ? (
          <div className="chat-empty">
            <h2>What can I help with?</h2>
            <p>Your conversations stay on this computer.</p>
          </div>
        ) : (
          p.state.messages.map((m) => (
            <Message key={m.id} m={m} streaming={m.id === p.state.streamingId} />
          ))
        )}
        {canRegenerate(p.state) && last !== undefined && (
          <div className="msg-actions">
            <button type="button" className="btn btn-ghost" onClick={p.onRegenerate}>
              Regenerate
            </button>
          </div>
        )}
      </div>
      {p.error !== null && (
        <div className="banner banner-error" role="alert">
          <span>{p.error}</span>
          {/settings/i.test(p.error) && (
            <button type="button" className="btn btn-ghost" onClick={p.onOpenSettings}>
              Open settings
            </button>
          )}
        </div>
      )}
      <form
        className="composer"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <label htmlFor="composer-input" className="visually-hidden">
          Message
        </label>
        <textarea
          id="composer-input"
          value={draft}
          rows={Math.min(8, Math.max(1, draft.split("\n").length))}
          placeholder="Message Demios… (Enter to send, Shift+Enter for a new line)"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKeyDown}
        />
        {streaming ? (
          <button type="button" className="btn btn-stop" onClick={p.onStop}>
            Stop
          </button>
        ) : (
          <button type="submit" className="btn btn-primary" disabled={draft.trim() === ""}>
            Send
          </button>
        )}
      </form>
    </section>
  );
}

function Message({ m, streaming }: { m: StoredMessage; streaming: boolean }) {
  const text = messageText(m);
  return (
    <article className={`msg msg-${m.role}`} aria-busy={streaming}>
      <div className="msg-role">{m.role === "user" ? "You" : "Demios"}</div>
      {m.role === "user" ? (
        <div className="msg-body msg-plain">{text}</div>
      ) : (
        <div className="msg-body">
          {text === "" && streaming ? (
            <span className="typing" aria-label="Thinking">
              <i />
              <i />
              <i />
            </span>
          ) : (
            <Markdown source={text} />
          )}
          {streaming && text !== "" && <span className="cursor" aria-hidden="true" />}
        </div>
      )}
      {m.status === "cancelled" && <div className="msg-note">Stopped.</div>}
      {m.status === "error" && (
        <div className="msg-note msg-note-error">{m.error ?? "The answer failed."}</div>
      )}
    </article>
  );
}
