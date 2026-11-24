import { ArrowUp, Copy, Flag, RotateCcw, Square } from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import {
  messageText,
  type ApprovalDecision,
  type ApprovalPrompt,
  type ArtifactType,
  type Id,
  type ModelCatalog,
  type StoredMessage,
  type UnknownOutcome,
} from "../ipc/types";
import { AI_DISCLOSURE, AI_FIRST_RUN_NOTICE } from "../ai";
import { describeModel } from "../models";
import { mod } from "../platform";
import { withoutArtifactBlocks } from "../fenced";
import { canRegenerate, type ChatState, type LiveToolCall } from "../state/chat";
import { ArtifactCard } from "./ArtifactPane";
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
  /** Conversation title (the page's level-one heading once messages show). */
  title: string;
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
  /** Artifacts each assistant message made (by message id), for the artifact cards. */
  artifactCards: Record<Id, ArtifactCardInfo[]>;
  onOpenArtifact: (id: Id) => void;
  /** Show the first-run AI notice (T-STORE-09). */
  aiNotice?: boolean;
  onDismissAiNotice?: () => void;
  /** The composer's text, kept by the parent across view switches. */
  draft?: string;
  onDraft?: (text: string) => void;
  /** "Report this answer": opens a form in the browser with the answer's model, no chat content. */
  onReportAnswer?: (messageId: Id) => void;
  /** A link in an answer was clicked. */
  onOpenLink?: (href: string) => void;
}

/** One artifact a message created or changed: its latest version from that message. */
export interface ArtifactCardInfo {
  id: Id;
  title: string;
  type: ArtifactType;
  version: number;
}

interface ToolResult {
  text: string;
  isError: boolean;
}

const EXAMPLES = [
  "Explain how a hash map works, with a short Rust example",
  "Draft a polite reply declining a meeting",
  "Give me a checklist for reviewing a pull request",
];

export function ChatView(p: Props) {
  const connectRef = useRef<HTMLButtonElement>(null);
  // The draft lives in App when it passes one, so switching to Settings (e.g. a demios://
  // link) and back keeps it.
  const [localDraft, setLocalDraft] = useState("");
  const draft = p.draft ?? localDraft;
  const setDraft = p.onDraft ?? setLocalDraft;
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
  const results = new Map<string, ToolResult>();
  for (const m of p.state.messages) {
    if (m.role === "tool" && m.tool_call_id !== null) {
      results.set(m.tool_call_id, { text: messageText(m), isError: m.status === "error" });
    }
  }

  return (
    <section className="chat" aria-label="Conversation">
      <div className="messages" ref={listRef}>
        <div className="column">
          {!empty && <h1 className="dn-visually-hidden">{p.title}</h1>}
          {p.aiNotice === true && (
            <InlineMessage
              tone="info"
              action={
                <Button
                  size="sm"
                  onClick={() => {
                    p.onDismissAiNotice?.();
                    // The notice goes away; don't drop focus to the page (§7).
                    const box = p.composerRef.current;
                    if (box !== null && !box.disabled) box.focus();
                    else connectRef.current?.focus();
                  }}
                >
                  Got it
                </Button>
              }
            >
              {AI_FIRST_RUN_NOTICE}
            </InlineMessage>
          )}
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
                  onReport={p.onReportAnswer}
                  onOpenLink={p.onOpenLink}
                  live={p.state.tools[m.id] ?? []}
                  results={results}
                  artifacts={p.artifactCards[m.id] ?? []}
                  onOpenArtifact={p.onOpenArtifact}
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
          data-testid="composer-input"
          ref={p.composerRef}
          value={draft}
          rows={Math.min(8, Math.max(1, draft.split("\n").length))}
          placeholder={p.ready ? "Message Demios" : "Connect a model to start chatting"}
          disabled={!p.ready}
          aria-describedby="composer-hint composer-disclosure"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="composer-bar">
          <span id="composer-hint" className="composer-hint">
            Enter to send · Shift+Enter for a new line
          </span>
          {streaming || sending ? (
            <IconButton
              label="Stop answer"
              shortcut={`${mod}.`}
              className="composer-stop"
              onClick={p.onStop}
              data-testid="stop-button"
            >
              <Square {...ICON} aria-hidden="true" />
            </IconButton>
          ) : p.ready ? (
            <IconButton
              label="Send message"
              data-testid="send-button"
              shortcut="Enter"
              className="composer-send"
              type="submit"
              aria-busy={sending || undefined}
            >
              <ArrowUp {...ICON} aria-hidden="true" />
            </IconButton>
          ) : (
            <Button ref={connectRef} variant="primary" size="sm" onClick={p.onConnect}>
              Connect a model
            </Button>
          )}
        </div>
      </form>
      <p id="composer-disclosure" className="composer-disclosure">
        {AI_DISCLOSURE}
      </p>
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
  onReport,
  onOpenLink,
  live,
  results,
  artifacts,
  onOpenArtifact,
}: {
  m: StoredMessage;
  catalog: ModelCatalog | null;
  streaming: boolean;
  isLast: boolean;
  canRetry: boolean;
  onRetry: () => void;
  onReport?: ((messageId: Id) => void) | undefined;
  onOpenLink?: ((href: string) => void) | undefined;
  live: LiveToolCall[];
  results: Map<string, ToolResult>;
  artifacts: ArtifactCardInfo[];
  onOpenArtifact: (id: Id) => void;
}) {
  const toast = useToast();
  const raw = messageText(m);
  // Stored calls (after the run) or live ones (while streaming).
  const calls =
    m.tool_calls.length > 0
      ? m.tool_calls.map((c) => ({
          call_id: c.id,
          name: c.name,
          arguments: c.arguments,
          // No stored result means the call never finished.
          status:
            results.get(c.id)?.isError === false ? ("done" as const) : ("error" as const),
          summary: results.get(c.id)?.text ?? "",
        }))
      : live;
  // Artifact tool calls that worked are shown as artifact cards instead of tool cards.
  const shownCalls = calls.filter((c) => !(c.name.startsWith("artifact_") && c.status === "done"));
  const usedArtifactTools = calls.some((c) => c.name.startsWith("artifact_"));
  // A tool-less model's saved `<artifact>` blocks are shown as cards, not as raw text.
  const text = artifacts.length > 0 && !usedArtifactTools ? withoutArtifactBlocks(raw) : raw;
  if (m.role === "user") {
    return (
      <article className="msg msg-user" aria-label="You" data-testid="message-user">
        <div className="bubble">{text}</div>
      </article>
    );
  }
  const retry = isLast && canRetry;
  return (
    <article
      className="msg msg-assistant"
      aria-label={`AI answer${m.model !== null ? ` from ${describeModel(m.model, catalog)?.text ?? m.model}` : ""}`}
      data-ai-generated="true"
      data-generator={m.model ?? undefined}
      aria-busy={streaming}
      data-testid="message-assistant"
      data-status={m.status}
    >
      {text === "" && streaming && calls.length === 0 ? (
        <span className="thinking">Thinking…</span>
      ) : (
        text !== "" && (
          <div className="msg-body">
            <Markdown source={text} {...(onOpenLink !== undefined ? { onOpenLink } : {})} />
            {streaming && <span className="caret" aria-hidden="true" />}
          </div>
        )
      )}
      {artifacts.map((a) => (
        <ArtifactCard key={a.id} title={a.title} type={a.type} version={a.version} onOpen={() => onOpenArtifact(a.id)} />
      ))}
      {shownCalls.map((c) => (
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
          {onReport !== undefined && (
            <IconButton size="sm" label="Report this answer" onClick={() => onReport(m.id)}>
              <Flag {...ICON} aria-hidden="true" />
            </IconButton>
          )}
          <ModelBadge info={describeModel(m.model, catalog)} />
        </footer>
      )}
    </article>
  );
}
