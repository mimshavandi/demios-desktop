import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import { errorMessage, type McpServerToolsView, type McpToolView, type ToolChoice } from "../ipc/types";
import { Badge, Button, InlineMessage, Select, Skeleton } from "./primitives";
import { useToast } from "./toast";
import { RiskBadge } from "./ToolCards";

const CHOICES: { value: Exclude<ToolChoice, "default">; label: string }[] = [
  { value: "always_ask", label: "Ask every time" },
  { value: "ask_once_per_chat", label: "Ask once per chat" },
  { value: "run_without_asking", label: "Run without asking" },
  { value: "off", label: "Off: models can't use it" },
];

function approvalText(t: McpToolView): string {
  if (t.disabled) return "Off";
  if (t.runs_without_asking) return "Runs without asking";
  return t.approval === "ask_once_per_conversation" ? "Asks once per chat" : "Asks every time";
}

/**
 * One server's tools (T-SHELL-11, issue #67): each tool's name and description as plain text,
 * the risk tier its server claims, whether it runs without asking, and the user's choice.
 * Tools that are new or changed since they were reviewed always ask until marked reviewed.
 * `version` changes when the server reconnects or its review count changes, and reloads.
 */
export function McpToolsPanel({
  api,
  serverId,
  serverName,
  version,
}: {
  api: Api;
  serverId: string;
  serverName: string;
  version: string;
}) {
  const toast = useToast();
  const [view, setView] = useState<McpServerToolsView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      setView(await api.listMcpTools(serverId));
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api, serverId]);

  useEffect(() => {
    void load();
  }, [load, version]);

  const act = async (key: string, what: string, f: () => Promise<McpServerToolsView>) => {
    setBusy(key);
    try {
      setView(await f());
    } catch (e) {
      toast({ tone: "error", title: `${what} ${errorMessage(e)}` });
    } finally {
      setBusy(null);
    }
  };

  // "Mark reviewed" goes away once done, so focus moves to the first reviewed tool's choice.
  const review = async (tools: McpToolView[]) => {
    await act(`review:${tools.length === 1 ? tools[0]?.name : "*"}`, "Couldn't mark them reviewed.", () =>
      api.markMcpToolsReviewed(
        serverId,
        tools.map((t) => ({ name: t.name, fingerprint: t.fingerprint })),
      ),
    );
    requestAnimationFrame(() => {
      const row = [...(panelRef.current?.querySelectorAll<HTMLElement>("[data-tool]") ?? [])].find(
        (el) => el.dataset.tool === tools[0]?.name,
      );
      row?.querySelector<HTMLElement>("[role=combobox], button")?.focus();
    });
  };

  const flagged = view?.tools.filter((t) => t.flag !== null) ?? [];

  return (
    <div className="mcp-tools" role="region" aria-label={`Tools of ${serverName}`} data-testid="mcp-tools" ref={panelRef}>
      {view === null && error === null && (
        <div aria-busy="true" aria-label="Loading tools">
          <Skeleton kind="block" />
        </div>
      )}
      {error !== null && (
        <InlineMessage
          tone="error"
          action={
            <Button size="sm" onClick={() => void load()}>
              Retry
            </Button>
          }
        >
          Couldn't load its tools. {error}
        </InlineMessage>
      )}
      {view !== null && !view.connected && <p className="dn-help">Connect the server to see its tools.</p>}
      {view !== null && view.connected && view.tools.length === 0 && <p className="dn-help">This server offers no tools.</p>}
      {flagged.length > 0 && (
        <InlineMessage
          tone="warning"
          action={
            flagged.length > 1 ? (
              <Button size="sm" loading={busy === "review:*"} onClick={() => void review(flagged)}>
                Mark all reviewed
              </Button>
            ) : undefined
          }
        >
          {flagged.length === 1 ? "1 tool is" : `${flagged.length} tools are`} new or changed since you last reviewed this
          server. They ask before every use until you mark them reviewed. Read the descriptions first: a server can use them
          to give models instructions.
        </InlineMessage>
      )}
      {view !== null && view.tools.length > 0 && (
        <ul className="mcp-tool-list">
          {view.tools.map((t) => (
            <ToolRow
              key={t.id}
              tool={t}
              busy={busy}
              onChoice={(choice) =>
                void act(`choice:${t.name}`, "Couldn't save that.", () => api.setMcpToolChoice(serverId, t.name, choice))
              }
              onReview={() => void review([t])}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function ToolRow({
  tool: t,
  busy,
  onChoice,
  onReview,
}: {
  tool: McpToolView;
  busy: string | null;
  onChoice: (c: ToolChoice) => void;
  onReview: () => void;
}) {
  const selectId = useId();
  const helpId = useId();
  return (
    <li className="mcp-tool" data-testid="mcp-tool" data-tool={t.name}>
      <div className="mcp-tool-head">
        <span className="mcp-tool-name">{t.name}</span>
        {t.title !== null && <span className="dn-help">{t.title}</span>}
        {t.flag !== null && <Badge tone="warning">{t.flag === "new" ? "New" : "Changed"}</Badge>}
      </div>
      {/* Plain text from the server: rendered as text, never as Markdown or HTML. */}
      <p className="mcp-tool-desc">{t.description === "" ? "No description." : t.description}</p>
      <div className="mcp-tool-meta">
        <span className="dn-help">Server says:</span>
        <RiskBadge risk={t.seeded_risk} />
        <span className="dn-help mcp-tool-now">Now:</span>
        <Badge tone={t.runs_without_asking ? "accent" : "neutral"}>{approvalText(t)}</Badge>
      </div>
      <div className="mcp-tool-actions">
        <label htmlFor={selectId} className="mcp-tool-label">
          When a model uses it
        </label>
        <Select
          id={selectId}
          value={t.choice === "default" ? "" : t.choice}
          placeholder="Follow its risk tier"
          emptyText=""
          groups={[{ label: "Your choice", options: CHOICES }]}
          {...(t.flag !== null ? { "aria-describedby": helpId } : {})}
          onValueChange={(v) => onChoice(v === "" ? "default" : (v as ToolChoice))}
        />
        {busy === `choice:${t.name}` && <span className="dn-help">Saving…</span>}
        {t.flag !== null && (
          <>
            <span id={helpId} className="dn-help">
              Asks every time until reviewed.
            </span>
            <Button size="sm" variant="secondary" loading={busy === `review:${t.name}`} onClick={onReview}>
              Mark reviewed
            </Button>
          </>
        )}
      </div>
    </li>
  );
}
