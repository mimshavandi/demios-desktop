import { ArrowUp, Copy, RotateCcw, Square } from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import {
  messageText,
  type ApprovalDecision,
  type ApprovalPrompt,
  type ModelCatalog,
  type StoredMessage,
  type UnknownOutcome,
} from "../ipc/types";
import { describeModel } from "../models";
import { mod } from "../platform";
import { canRegenerate, type ChatState, type LiveToolCall } from "../state/chat";
import { Markdown } from "./Markdown";
import { ModelBadge } from "./ModelBadge";
import { Button, ICON, IconButton, InlineMessage, Skeleton } from "./primitives";
import { ApprovalCard, ToolCallCard, UnknownOutcomeCard } from "./ToolCards";
import { useToast } from "./toast";

interface Props {
  state: ChatState;
  loading: boolean;
  /** A chat model is available; otherwise the composer shows "Connect a model". */
  ready: boolean;
  catalog: ModelCatalog | null;
  /** Model the next answer will use. */
  model: string | null;
  error: string | null;
  composerRef: RefObject<HTMLTextAreaElement | null>;
  onSend: (text: string) => Promise<boolean>;
  onStop: () => void;
  onRegenerate: () => void;
  onConnect: () => void;
  /** Approval requests of this conversation (shown inline, never auto-allowed). */
  approvals: ApprovalPrompt[];
  onDecide: (requestId: string, d: ApprovalDecision) => Promise<void>;
  /** Tool writes of this conversation whose outcome is unknown. */
  unknowns: UnknownOutcome[];
  onResolveUnknown: (key: string, happened: boolean) => Promise<void>;
}

const EXAMPLES = [
  "Explain how a hash map works, with a short Rust example",
  "Draft a polite reply declining a meeting",
  "Give me a checklist for reviewing a pull request",
];

export function ChatView(p: Props) {
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const listRef = useRef<HTMLDivElement>(null);
  const streaming = p.state.streamingId !== null;
  const lastId = p.state.messages[p.state.messages.length - 1]?.id;

  // Follow the stream while the reader is near the bottom.
  useEffect(() => {
    const el = listRef.current;
    if (el === null) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 160) el.scrollTop = el.scrollHeight;
  }, [p.state.messages]);

  const submit = async (text = draft.trim()) => {
    if (text === "" || streaming || sending || !p.ready) return;
    setSending(true);
    setDraft("");
    const ok = await p.onSend(text);
    // Keep the prompt when sending failed.
    if (!ok) setDraft(text);
    setSending(false);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void submit();
    }
  };

  const empty = !p.loading && p.state.messages.length === 0;
  // Tool results are shown inside their call's card, not as messages.
  const results = new Map<string, string>();
  for (const m of p.state.messages) {
    if (m.role === "tool" && m.tool_call_id !== null) results.set(m.tool_call_id, messageText(m));
  }

  return (
    <section className="chat" aria-label="Conversation">
      <div className="messages" ref={listRef}>
        <div className="column">
          {!p.ready && (
            <InlineMessage
              tone="warning"
              action={
                <Button size="sm" onClick={p.onConnect}>
                  Connect a model
                </Button>
              }
            >
              No model connected. Download a model or add a provider key to start chatting.
            </InlineMessage>
          )}
          {p.loading && (
            <div className="msg-skeleton" aria-busy="true" aria-label="Loading messages">
              <Skeleton width="40%" />
              <Skeleton kind="block" />
              <Skeleton width="55%" />
            </div>
          )}
          {empty && (
            <div className="welcome">
              <h1>What can I help with?</h1>
              <div className="welcome-model">
                <ModelBadge info={p.ready ? describeModel(p.model, p.catalog) : null} />
              </div>
              <ul className="examples" aria-label="Example prompts">
                {EXAMPLES.map((ex) => (
                  <li key={ex}>
                    <button
                      type="button"
                      className="example"
                      disabled={!p.ready}
                      onClick={() => void submit(ex)}
                    >
                      {ex}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {!p.loading &&
            p.state.messages
              .filter((m) => m.role !== "tool" && m.role !== "system")
              .map((m) => (
                <Message
                  key={m.id}
                  m={m}
                  catalog={p.catalog}
                  streaming={m.id === p.state.streamingId}
                  isLast={m.id === lastId}
                  canRetry={canRegenerate(p.state)}
                  onRetry={p.onRegenerate}
                  live={p.state.tools[m.id] ?? []}
                  results={results}
                />
              ))}
          {p.approvals.map((a) => (
            <ApprovalCard
              key={a.request_id}
              prompt={a}
              onDecide={(d) => p.onDecide(a.request_id, d)}
            />
          ))}
          {p.unknowns.map((u) => (
            <UnknownOutcomeCard
              key={u.key}
              outcome={u}
              onResolve={(happened) => p.onResolveUnknown(u.key, happened)}
            />
          ))}
          {p.error !== null && (
            <InlineMessage tone="error">
              {p.error}
            </InlineMessage>
          )}
        </div>
      </div>

      <form
        className={`composer${p.ready ? "" : " is-disabled"}`}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <label htmlFor="composer-input" className="dn-visually-hidden">
          Message
        </label>
        <textarea
          id="composer-input"
          ref={p.composerRef}
          value={draft}
          rows={Math.min(8, Math.max(1, draft.split("\n").length))}
          placeholder={p.ready ? "Message Demios" : "Connect a model to start chatting"}
          disabled={!p.ready}
          aria-describedby="composer-hint"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="composer-bar">
          <span id="composer-hint" className="composer-hint">
            Enter to send · Shift+Enter for a new line
          </span>
          {streaming || sending ? (
            <IconButton label="Stop answer" shortcut={`${mod}.`} className="composer-stop" onClick={p.onStop}>
              <Square {...ICON} aria-hidden="true" />
            </IconButton>
          ) : p.ready ? (
            <IconButton
              label="Send message"
              shortcut="Enter"
              className="composer-send"
              type="submit"
              aria-busy={sending || undefined}
            >
              <ArrowUp {...ICON} aria-hidden="true" />
            </IconButton>
          ) : (
            <Button variant="primary" size="sm" onClick={p.onConnect}>
              Connect a model
            </Button>
          )}
        </div>
      </form>
    </section>
  );
}

function Message({
  m,
  catalog,
  streaming,
  isLast,
  canRetry,
  onRetry,
  live,
  results,
}: {
  m: StoredMessage;
  catalog: ModelCatalog | null;
  streaming: boolean;
  isLast: boolean;
  canRetry: boolean;
  onRetry: () => void;
  live: LiveToolCall[];
  results: Map<string, string>;
}) {
  const toast = useToast();
  const text = messageText(m);
  // Stored calls (after the run) or live ones (while streaming).
  const calls =
    m.tool_calls.length > 0
      ? m.tool_calls.map((c) => ({
          call_id: c.id,
          name: c.name,
          arguments: c.arguments,
          status: results.has(c.id) ? ("done" as const) : ("error" as const),
          summary: results.get(c.id) ?? "",
        }))
      : live;
  if (m.role === "user") {
    return (
      <article className="msg msg-user" aria-label="You">
        <div className="bubble">{text}</div>
      </article>
    );
  }
  const retry = isLast && canRetry;
  return (
    <article className="msg msg-assistant" aria-label="Demios" aria-busy={streaming}>
      {text === "" && streaming && calls.length === 0 ? (
        <span className="thinking">Thinking…</span>
      ) : (
        text !== "" && (
          <div className="msg-body">
            <Markdown source={text} />
            {streaming && <span className="caret" aria-hidden="true" />}
          </div>
        )
      )}
      {calls.map((c) => (
        <ToolCallCard
          key={c.call_id}
          name={c.name}
          args={c.arguments}
          status={c.status}
          result={c.summary}
        />
      ))}
      {m.status === "complete" && m.finish_reason === "length" && isLast && (
        <InlineMessage tone="info">The answer hit the length limit.</InlineMessage>
      )}
      {m.status === "complete" && m.finish_reason === "tool_calls" && isLast && (
        <InlineMessage tone="info">Stopped after the maximum number of tool steps.</InlineMessage>
      )}
      {m.status === "cancelled" && (
        <InlineMessage
          tone="info"
          action={
            retry ? (
              <Button size="sm" icon={<RotateCcw {...ICON} aria-hidden="true" />} onClick={onRetry}>
                Retry
              </Button>
            ) : undefined
          }
        >
          Stopped
        </InlineMessage>
      )}
      {m.status === "error" && (
        <InlineMessage
          tone="error"
          action={
            retry ? (
              <Button size="sm" icon={<RotateCcw {...ICON} aria-hidden="true" />} onClick={onRetry}>
                Retry
              </Button>
            ) : undefined
          }
        >
          {m.error ?? "The answer failed."}
        </InlineMessage>
      )}
      {!streaming && text !== "" && (
        <footer className="msg-footer">
          <IconButton
            size="sm"
            label="Copy answer"
            onClick={() =>
              void navigator.clipboard
                ?.writeText(text)
                .then(() => toast({ tone: "success", title: "Answer copied" }))
            }
          >
            <Copy {...ICON} aria-hidden="true" />
          </IconButton>
          {retry && m.status === "complete" && (
            <IconButton size="sm" label="Retry" onClick={onRetry}>
              <RotateCcw {...ICON} aria-hidden="true" />
            </IconButton>
          )}
          <ModelBadge info={describeModel(m.model, catalog)} />
        </footer>
      )}
    </article>
  );
}
