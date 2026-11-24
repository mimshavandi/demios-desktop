// Tool call, approval and outcome-unknown cards (design-system §4, chat-specific).
// Approval rules: the title says the action, the exact arguments are always shown, risk has
// a badge, "Always allow in this chat" is hidden for destructive/sensitive tools, ⌘↵ allows,
// Esc denies, and nothing ever auto-allows (an unanswered request is denied by the backend).

import { Check, ChevronRight, CircleAlert, LoaderCircle, ShieldAlert, Wrench } from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import type { ApprovalDecision, ApprovalPrompt, RiskTier, UnknownOutcome } from "../ipc/types";
import { isMac, mod } from "../platform";
import { Badge, Button, ICON } from "./primitives";

const SMALL = { size: 12, strokeWidth: 1.5 } as const;

/** `mcp__files__write_file` → server `files`, tool `write_file`. */
export function toolLabel(name: string): { server: string | null; tool: string } {
  const m = /^mcp__(.+?)__(.+)$/.exec(name);
  return m ? { server: m[1] ?? null, tool: m[2] ?? name } : { server: null, tool: name };
}

const RISK: Record<RiskTier, { label: string; tone: "neutral" | "accent" | "warning" | "danger" }> = {
  read: { label: "Reads data", tone: "neutral" },
  low_write: { label: "Changes local data", tone: "accent" },
  external_write: { label: "Acts outside the app", tone: "warning" },
  destructive: { label: "Can delete or overwrite", tone: "danger" },
  sensitive: { label: "Sensitive", tone: "danger" },
};

export function RiskBadge({ risk }: { risk: RiskTier }) {
  const r = RISK[risk];
  return (
    <Badge tone={r.tone} icon={<ShieldAlert {...SMALL} aria-hidden="true" />}>
      {r.label}
    </Badge>
  );
}

function Args({ value }: { value: unknown }) {
  const text = JSON.stringify(value ?? {}, null, 2);
  return <pre className="tool-args">{text}</pre>;
}

export type ToolCallStatus = "running" | "done" | "error";

export function ToolCallCard({
  name,
  args,
  status,
  result,
}: {
  name: string;
  args: unknown;
  status: ToolCallStatus;
  result: string;
}) {
  const [open, setOpen] = useState(false);
  const [started] = useState(() => Date.now());
  const [now, setNow] = useState(started);
  useEffect(() => {
    if (status !== "running") return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [status]);
  const { server, tool } = toolLabel(name);
  const secs = Math.max(0, Math.round((now - started) / 1000));
  return (
    <div className={`tool-card tool-${status}`} data-testid="tool-call-card">
      <button
        type="button"
        className="tool-head"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <ChevronRight {...ICON} className="tool-chevron" aria-hidden="true" />
        <Wrench {...ICON} aria-hidden="true" />
        <span className="tool-name">
          {server !== null && <span className="tool-server">{server} · </span>}
          {tool}
        </span>
        {status === "running" && (
          <span className="tool-status">
            <LoaderCircle {...SMALL} className="dn-spin" aria-hidden="true" /> Running {secs} s
          </span>
        )}
        {status === "done" && (
          <span className="tool-status">
            <Check {...SMALL} aria-hidden="true" /> Done
          </span>
        )}
        {status === "error" && (
          <span className="tool-status tool-status-error">
            <CircleAlert {...SMALL} aria-hidden="true" /> Failed
          </span>
        )}
      </button>
      {open && (
        <div className="tool-body">
          <div className="tool-label">Arguments</div>
          <Args value={args} />
          {result !== "" && (
            <>
              <div className="tool-label">Result</div>
              <pre className="tool-args">{result}</pre>
            </>
          )}
        </div>
      )}
    </div>
  );
}

export function ApprovalCard({
  prompt,
  onDecide,
}: {
  prompt: ApprovalPrompt;
  onDecide: (d: ApprovalDecision) => Promise<void>;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [busy, setBusy] = useState<ApprovalDecision | null>(null);
  const canRemember =
    prompt.policy === "ask_once_per_conversation" &&
    prompt.risk !== "destructive" &&
    prompt.risk !== "sensitive";
  const titleId = `approval-${prompt.request_id}`;

  // The approval is the next thing to act on: take focus (design-system §7).
  useEffect(() => {
    ref.current?.focus();
  }, []);

  const decide = async (d: ApprovalDecision) => {
    if (busy !== null) return;
    setBusy(d);
    try {
      await onDecide(d);
    } finally {
      setBusy(null);
    }
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const m = isMac ? e.metaKey : e.ctrlKey;
    if (m && e.key === "Enter") {
      e.preventDefault();
      void decide("approve");
    } else if (e.key === "Escape") {
      e.preventDefault();
      void decide("deny");
    }
  };

  const { server, tool } = toolLabel(prompt.capability_id.replace(/^mcp\.([^.]+)\./, "mcp__$1__"));
  return (
    <div
      ref={ref}
      className="approval-card"
      role="group"
      aria-labelledby={titleId}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      data-testid="approval-card"
    >
      <div className="approval-head">
        <h3 id={titleId}>{prompt.title}</h3>
        <RiskBadge risk={prompt.risk} />
      </div>
      {prompt.description !== "" && <p className="dn-help">{prompt.description}</p>}
      <div className="tool-label">
        {server !== null ? `${server} · ${tool}` : prompt.capability_id} will receive
      </div>
      <Args value={prompt.arguments} />
      <div className="approval-actions">
        <Button
          variant="primary"
          loading={busy === "approve"}
          onClick={() => void decide("approve")}
          aria-keyshortcuts={`${isMac ? "Meta" : "Control"}+Enter`}
        >
          Allow once <kbd className="on-accent">{mod}↵</kbd>
        </Button>
        {canRemember && (
          <Button
            loading={busy === "approve_for_conversation"}
            onClick={() => void decide("approve_for_conversation")}
          >
            Always allow in this chat
          </Button>
        )}
        <Button
          variant="ghost"
          loading={busy === "deny"}
          onClick={() => void decide("deny")}
          aria-keyshortcuts="Escape"
        >
          Deny <kbd>Esc</kbd>
        </Button>
      </div>
    </div>
  );
}

export function UnknownOutcomeCard({
  outcome,
  onResolve,
}: {
  outcome: UnknownOutcome;
  onResolve: (happened: boolean) => Promise<void>;
}) {
  const [busy, setBusy] = useState<boolean | null>(null);
  const resolve = async (happened: boolean) => {
    setBusy(happened);
    try {
      await onResolve(happened);
    } finally {
      setBusy(null);
    }
  };
  return (
    <div className="approval-card unknown-card" role="group" aria-label="Outcome unknown" data-testid="unknown-outcome-card">
      <div className="approval-head">
        <h3>Did this go through?</h3>
        <Badge tone="warning">Outcome unknown</Badge>
      </div>
      <p className="dn-help">
        <code>{outcome.capability_id}</code> was interrupted, so it may or may not have happened.
        Check, then tell the app. It won't run this again until you do.
      </p>
      <Args value={outcome.arguments} />
      <div className="approval-actions">
        <Button loading={busy === true} onClick={() => void resolve(true)}>
          It went through
        </Button>
        <Button variant="ghost" loading={busy === false} onClick={() => void resolve(false)}>
          It didn't happen
        </Button>
      </div>
    </div>
  );
}
