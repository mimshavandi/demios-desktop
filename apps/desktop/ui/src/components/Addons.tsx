import { ChevronDown, ChevronLeft, ChevronRight, CircleOff, Hand, OctagonAlert, ShieldAlert, ShieldQuestion } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type AddonConsentRequest,
  type AddonDetail,
  type AddonJob,
  type AddonPermissionView,
  type AddonSummary,
  type HeldCall,
  type MissedRunRule,
} from "../ipc/types";
import { AddonConsent } from "./AddonConsent";
import { permissionHelp, permissionLabel, ruleLabel, runText, scheduleText, sortPermissions, timeText } from "./addonText";
import { Badge, Button, ICON, InlineMessage, Select, Skeleton, Switch } from "./primitives";
import { useToast } from "./toast";

const BADGE_ICON = { size: 12, strokeWidth: 1.5 } as const;

type Load<T> = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; value: T };

function useLoad<T>(fetch: () => Promise<T>) {
  const [load, setLoad] = useState<Load<T>>({ kind: "loading" });
  const reload = useCallback(
    async (quiet = false) => {
      if (!quiet) setLoad({ kind: "loading" });
      try {
        setLoad({ kind: "ready", value: await fetch() });
      } catch (e) {
        setLoad({ kind: "error", message: errorMessage(e) });
      }
    },
    [fetch],
  );
  return [load, reload] as const;
}

/**
 * Settings › Add-ons (design system: AddonsSettings): the add-ons built into this copy, and
 * one add-on's page with what it may do and its background jobs. Shown only in a build
 * with add-ons.
 */
export function AddonsPage({ api }: { api: Api }) {
  const [open, setOpen] = useState<string | null>(null);
  const [list, reloadList] = useLoad(useCallback(() => api.addonsList(), [api]));
  const [paused, setPaused] = useState<boolean | null>(null);
  const toast = useToast();
  const openButtons = useRef(new Map<string, HTMLButtonElement>());
  const [version, setVersion] = useState(0);

  useEffect(() => {
    void reloadList();
    api
      .addonsPaused()
      .then(setPaused)
      .catch(() => setPaused(null));
    // Jobs ran, calls were held or answered: reload what is shown.
    void api.addonsWatch(() => {
      void reloadList(true);
      setVersion((v) => v + 1);
    });
  }, [api, reloadList]);

  const back = () => {
    const id = open;
    setOpen(null);
    void reloadList(true);
    requestAnimationFrame(() => (id === null ? undefined : openButtons.current.get(id)?.focus()));
  };

  if (open !== null) {
    return <AddonPage api={api} id={open} version={version} onBack={back} />;
  }

  const anyJobs = list.kind === "ready" && list.value.some((a) => a.has_jobs);
  return (
    <section className="card" aria-label="Add-ons in this copy of Almend" aria-busy={list.kind === "loading"}>
      <p className="dn-help">
        Built into this copy of Almend. They follow the same rules as everything else: tools ask before they change
        things, and Delete all local data removes their data too.
      </p>
      {anyJobs && paused !== null && (
        <Switch
          label="Pause all background jobs"
          description="Stops every job, catch-up run and pending “Run now?” question until you turn it back on."
          checked={paused}
          onCheckedChange={(v) => {
            setPaused(v);
            api.addonsSetPaused(v).catch((e: unknown) => {
              setPaused(!v);
              toast({ tone: "error", title: `Couldn't change that. ${errorMessage(e)}` });
            });
          }}
        />
      )}
      {list.kind === "loading" && (
        <>
          <div className="row">
            <div className="row-main">
              <Skeleton width="40%" />
            </div>
          </div>
          <div className="row">
            <div className="row-main">
              <Skeleton width="35%" />
            </div>
          </div>
        </>
      )}
      {list.kind === "error" && (
        <InlineMessage
          tone="error"
          action={
            <Button size="sm" onClick={() => void reloadList()}>
              Retry
            </Button>
          }
        >
          Couldn't load your add-ons. {list.message}
        </InlineMessage>
      )}
      {list.kind === "ready" && (
        <ul className="addon-list">
          {list.value.map((a) => (
            <li key={a.id} className="row" data-testid={`addon-row-${a.id}`}>
              <div className="row-main">
                <span className="row-title">
                  {a.name} <StatusBadge a={a} paused={paused === true} />
                </span>
                <span className="dn-help">{a.state === "not_running" && a.reason !== null ? a.reason : `Version ${a.version}`}</span>
              </div>
              <div className="row-actions">
                <Button
                  variant="ghost"
                  size="sm"
                  aria-label={`Open ${a.name}`}
                  ref={(el) => {
                    if (el === null) openButtons.current.delete(a.id);
                    else openButtons.current.set(a.id, el);
                  }}
                  onClick={() => setOpen(a.id)}
                >
                  Open
                  <ChevronRight {...ICON} aria-hidden="true" />
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function StatusBadge({ a, paused }: { a: AddonSummary; paused: boolean }) {
  if (a.state === "not_running")
    return (
      <Badge tone="danger" icon={<OctagonAlert {...BADGE_ICON} aria-hidden="true" />}>
        Not running
      </Badge>
    );
  if (a.state === "off")
    return (
      <Badge icon={<CircleOff {...BADGE_ICON} aria-hidden="true" />}>
        Off
      </Badge>
    );
  if (a.waiting > 0)
    return (
      <Badge tone="warning" icon={<Hand {...BADGE_ICON} aria-hidden="true" />}>
        Waiting for you · {a.waiting}
      </Badge>
    );
  if (a.needs_consent)
    return (
      <Badge tone="warning" icon={<ShieldQuestion {...BADGE_ICON} aria-hidden="true" />}>
        Needs your OK
      </Badge>
    );
  if (paused && a.has_jobs) return <Badge>Paused</Badge>;
  return null;
}

function AddonPage({ api, id, version, onBack }: { api: Api; id: string; version: number; onBack: () => void }) {
  const toast = useToast();
  const [detail, reload] = useLoad(useCallback(() => api.addonDetail(id), [api, id]));
  const [consent, setConsent] = useState<AddonConsentRequest | null>(null);
  const [announce, setAnnounce] = useState("");
  const heading = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    void reload(version > 0);
  }, [reload, version]);
  useEffect(() => heading.current?.focus(), [detail.kind === "ready"]);

  const act = async (what: Promise<unknown>, done?: string) => {
    try {
      await what;
      if (done !== undefined) setAnnounce(done);
    } catch (e) {
      toast({ tone: "error", title: errorMessage(e) });
    }
    await reload(true);
  };

  const askConsent = async () => {
    try {
      const pending = await api.addonsPendingConsent();
      const mine = pending.find((p) => p.id === id);
      if (mine !== undefined) setConsent(mine);
    } catch (e) {
      toast({ tone: "error", title: errorMessage(e) });
    }
  };

  const backButton = (
    <Button variant="ghost" size="sm" onClick={onBack} icon={<ChevronLeft {...ICON} aria-hidden="true" />}>
      Add-ons
    </Button>
  );

  if (detail.kind === "loading")
    return (
      <div className="addon-page" aria-busy="true">
        {backButton}
        <div className="card">
          <Skeleton width="30%" />
          <Skeleton kind="block" />
        </div>
      </div>
    );
  if (detail.kind === "error")
    return (
      <div className="addon-page">
        {backButton}
        <InlineMessage
          tone="error"
          action={
            <Button size="sm" onClick={() => void reload()}>
              Retry
            </Button>
          }
        >
          Couldn't load this add-on. {detail.message}
        </InlineMessage>
      </div>
    );

  const d: AddonDetail = detail.value;
  const a = d.summary;
  const running = a.state !== "not_running";
  const newPerms = d.permissions.filter((p) => !p.asked).length;

  return (
    <div className="addon-page" data-testid={`addon-page-${a.id}`}>
      {backButton}
      <div className="addon-head">
        <div>
          <h2 ref={heading} tabIndex={-1} className="addon-title">
            {a.name}
          </h2>
          <p className="dn-help">Version {a.version}</p>
        </div>
        {running && (
          <Switch
            label={`Use ${a.name}`}
            checked={a.state === "running"}
            onCheckedChange={(v) => void act(api.addonSetEnabled(a.id, v), v ? `${a.name} is on` : `${a.name} is off`)}
          />
        )}
      </div>
      {!running && a.reason !== null && <InlineMessage tone="error">{a.reason}</InlineMessage>}

      {d.permissions.length > 0 && (
        <section className="card" aria-labelledby="addon-perms-h">
          <h3 id="addon-perms-h">What it may do</h3>
          {running && a.needs_consent && newPerms > 0 && (
            <InlineMessage
              tone="warning"
              action={
                <Button size="sm" onClick={() => void askConsent()}>
                  Review
                </Button>
              }
            >
              {d.permissions.every((p) => !p.asked)
                ? `${a.name} hasn't been allowed anything yet.`
                : `Version ${a.version} asks for ${newPerms} more permission${newPerms === 1 ? "" : "s"}.`}
            </InlineMessage>
          )}
          {sortPermissions(d.permissions).map((p) => (
            <PermissionRow
              key={p.permission}
              view={p}
              detail={d}
              disabled={!running}
              onChange={(on) => {
                if (on && !p.asked) void askConsent();
                else
                  void act(
                    api.addonSetPermission(a.id, p.permission, on),
                    `${permissionLabel(p.permission)} ${on ? "turned on" : "turned off"}`,
                  );
              }}
            />
          ))}
        </section>
      )}

      {d.jobs.length > 0 && (
        <section className="card" aria-labelledby="addon-jobs-h">
          <h3 id="addon-jobs-h">Background jobs</h3>
          <p className="dn-help">Jobs run while Almend is open. They never approve anything on their own.</p>
          {d.jobs.map((j) => (
            <JobRow
              key={j.id}
              job={j}
              held={d.held.filter((h) => h.job_id === j.id)}
              addonName={a.name}
              disabled={!running || a.state === "off"}
              onEnabled={(on) => void act(api.addonSetJobEnabled(a.id, j.id, on), `${j.name} ${on ? "on" : "off"}`)}
              onRule={(r) => void act(api.addonSetMissedRule(a.id, j.id, r))}
              onAnswer={(runNow) =>
                void act(api.addonAnswerQuestion(a.id, j.id, runNow), runNow ? `${j.name} runs now` : `${j.name} skipped`)
              }
              onHeld={(h, approve) =>
                void act(api.addonAnswerHeld(h.id, approve, h.arguments_sha256), approve ? "Allowed once" : "Dismissed")
              }
            />
          ))}
        </section>
      )}

      <div className="dn-visually-hidden" role="status" aria-live="polite">
        {announce}
      </div>
      {consent !== null && (
        <AddonConsent
          api={api}
          request={consent}
          onDone={() => {
            setConsent(null);
            void reload(true);
          }}
        />
      )}
    </div>
  );
}

function PermissionRow({
  view,
  detail,
  disabled,
  onChange,
}: {
  view: AddonPermissionView;
  detail: AddonDetail;
  disabled: boolean;
  onChange: (on: boolean) => void;
}) {
  let help = permissionHelp(view);
  if (view.permission === "chats.read" && view.allowed)
    help = detail.chats_all
      ? "Its messages and titles, in all chats."
      : `Its messages and titles, in the chats of ${detail.chats_projects.length} chosen project${detail.chats_projects.length === 1 ? "" : "s"}.`;
  if (view.permission === "knowledge.search" && view.allowed)
    help = `In ${detail.knowledge_projects.length} chosen project${detail.knowledge_projects.length === 1 ? "" : "s"}.`;
  return (
    <Switch
      label={permissionLabel(view.permission)}
      description={help}
      checked={view.allowed}
      disabled={disabled}
      onCheckedChange={onChange}
    />
  );
}

const RULES: { value: "catch_up_once" | "ask" | "skip"; rule: (proposed: MissedRunRule) => MissedRunRule }[] = [
  {
    value: "catch_up_once",
    rule: (p) => (p.rule === "catch_up_once" ? p : { rule: "catch_up_once", fresh_for_secs: 6 * 3600 }),
  },
  { value: "ask", rule: () => ({ rule: "ask" }) },
  { value: "skip", rule: () => ({ rule: "skip" }) },
];

function JobRow({
  job,
  held,
  addonName,
  disabled,
  onEnabled,
  onRule,
  onAnswer,
  onHeld,
}: {
  job: AddonJob;
  held: HeldCall[];
  addonName: string;
  disabled: boolean;
  onEnabled: (on: boolean) => void;
  onRule: (r: MissedRunRule | null) => void;
  onAnswer: (runNow: boolean) => void;
  onHeld: (h: HeldCall, approve: boolean) => void;
}) {
  const [history, setHistory] = useState(false);
  const latest = job.history[0];
  const failed = latest?.outcome === "failed" || latest?.outcome === "timed_out";
  const waiting = held.length > 0 || job.question !== null;
  const ruleId = `job-rule-${job.id}`;
  return (
    <div className="row addon-job" data-testid={`addon-job-${job.id}`}>
      <div className="row-main">
        <span className="row-title">
          {job.name}{" "}
          {waiting && (
            <Badge tone="warning" icon={<Hand {...BADGE_ICON} aria-hidden="true" />}>
              Waiting for you
            </Badge>
          )}
          {!waiting && failed && (
            <Badge tone="danger" icon={<OctagonAlert {...BADGE_ICON} aria-hidden="true" />}>
              Failed
            </Badge>
          )}
        </span>
        <span className="dn-help">{scheduleText(job.schedule)}</span>
        <span className="dn-help">
          {latest === undefined ? (job.enabled ? "Hasn't run yet" : "Off") : runText(latest)}
        </span>
        <div className="addon-job-controls">
          <label className="dn-help" htmlFor={ruleId}>
            If missed
          </label>
          <Select
            id={ruleId}
            value={job.missed_rule.rule}
            placeholder={`The add-on's choice: ${ruleLabel(job.proposed_rule)}`}
            emptyText=""
            groups={[
              {
                label: "If missed",
                options: RULES.map((r) => ({ value: r.value, label: ruleLabel(r.rule(job.proposed_rule)) })),
              },
            ]}
            onValueChange={(v) => {
              const r = RULES.find((x) => x.value === v);
              onRule(r === undefined ? null : r.rule(job.proposed_rule));
            }}
          />
          {job.history.length > 0 && (
            <Button
              variant="ghost"
              size="sm"
              aria-expanded={history}
              aria-label={`History of ${job.name}`}
              icon={
                history ? <ChevronDown {...ICON} aria-hidden="true" /> : <ChevronRight {...ICON} aria-hidden="true" />
              }
              onClick={() => setHistory((h) => !h)}
            >
              History
            </Button>
          )}
        </div>
        {history && (
          <ul className="addon-history">
            {job.history.map((r) => (
              <li key={r.id} className="dn-help">
                {runText(r)}
              </li>
            ))}
          </ul>
        )}
        {job.question !== null && (
          <InlineMessage
            tone="warning"
            action={
              <>
                <Button size="sm" onClick={() => onAnswer(true)}>
                  Run now
                </Button>
                <Button size="sm" variant="ghost" onClick={() => onAnswer(false)}>
                  Skip
                </Button>
              </>
            }
          >
            {job.name} missed {job.question.missed} run{job.question.missed === 1 ? "" : "s"} while Almend was closed.
            Run it now?
          </InlineMessage>
        )}
        {held.map((h) => (
          <div key={h.id} className="approval-card" role="group" aria-labelledby={`held-${h.id}`}>
            <p className="addon-who">
              {addonName} · {job.name} wants to
            </p>
            <div className="approval-head">
              <h4 id={`held-${h.id}`}>{h.summary}</h4>
              <Badge tone="warning" icon={<ShieldAlert {...BADGE_ICON} aria-hidden="true" />}>
                Held since {timeText(h.created_at)}
              </Badge>
            </div>
            <div className="tool-label">{h.tool} will receive</div>
            <pre className="tool-args">{prettyArgs(h.arguments)}</pre>
            <div className="approval-actions">
              <Button variant="primary" onClick={() => onHeld(h, true)}>
                Allow once
              </Button>
              <Button variant="ghost" onClick={() => onHeld(h, false)}>
                Dismiss
              </Button>
            </div>
          </div>
        ))}
      </div>
      <div className="row-actions">
        <Switch label={`Run ${job.name}`} checked={job.enabled} disabled={disabled} onCheckedChange={onEnabled} />
      </div>
    </div>
  );
}

function prettyArgs(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}
