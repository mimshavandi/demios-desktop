import { Check, ChevronDown, ChevronRight, ExternalLink, LogIn, Plug, RotateCcw, TriangleAlert } from "lucide-react";
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import { errorMessage, type ConnectEntryView, type ConnectView, type McpServerStatus } from "../ipc/types";
import { Badge, Button, ICON, InlineMessage, Skeleton } from "./primitives";

/**
 * Settings › Tools › Connect (ADR-0018; design: ConnectList). Reviewed services that run their
 * own tool server. Connect adds an ordinary tool server through the same path as the Add form,
 * then signs in in the browser; its tools keep their risk levels and approvals.
 *
 * `version` changes when the tool servers below change (and the other way round, `onChanged`).
 */
export function ConnectCard({
  api,
  live,
  version,
  onChanged,
}: {
  api: Api;
  live: McpServerStatus[];
  version: number;
  onChanged: () => void;
}) {
  const [view, setView] = useState<ConnectView | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  // Connects the person cancelled: their sign-in ends later with an error nobody needs to see.
  const cancelled = useRef(new Set<string>());

  const load = useCallback(async () => {
    try {
      setView(await api.connectCatalogue());
      setLoadError(null);
    } catch (e) {
      setView(null);
      setLoadError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load, version]);

  // One live region for the card: says when a service's state changes.
  const [announce, setAnnounce] = useState("");
  const seen = useRef<Map<string, string> | null>(null);
  useEffect(() => {
    if (view === null) return;
    const now = new Map<string, string>();
    for (const e of view.entries) {
      const st = live.find((l) => l.server_id === e.server_id);
      if (st) now.set(e.id, st.state);
    }
    const before = seen.current;
    seen.current = now;
    if (before === null) return;
    const said: string[] = [];
    for (const e of view.entries) {
      const s = now.get(e.id);
      if (s !== undefined && before.get(e.id) !== s) said.push(`${e.service}: ${stateWords(s)}`);
    }
    if (said.length > 0) setAnnounce(said.join(". "));
  }, [live, view]);

  const setError = (id: string, msg: string | null) =>
    setErrors((prev) => {
      const next = { ...prev };
      if (msg === null) delete next[id];
      else next[id] = msg;
      return next;
    });

  const signIn = async (e: ConnectEntryView, serverId: string) => {
    cancelled.current.delete(serverId);
    try {
      await api.signInMcpServer(serverId);
    } catch (err) {
      if (!cancelled.current.has(serverId)) setError(e.id, `Couldn't add ${e.service}: ${errorMessage(err)}`);
    }
  };

  const connect = async (e: ConnectEntryView) => {
    setBusy(e.id);
    setError(e.id, null);
    try {
      let serverId = e.server_id;
      if (serverId === null) {
        const v = await api.connectService(e.id);
        setView(v);
        onChanged();
        serverId = v.entries.find((x) => x.id === e.id)?.server_id ?? null;
      }
      if (serverId !== null) await signIn(e, serverId);
    } catch (err) {
      setError(e.id, `Couldn't add ${e.service}: ${errorMessage(err)}`);
    } finally {
      setBusy(null);
    }
  };

  const cancel = async (e: ConnectEntryView) => {
    if (e.server_id === null) return;
    cancelled.current.add(e.server_id);
    try {
      await api.removeMcpServer(e.server_id);
      await load();
      onChanged();
    } catch (err) {
      setError(e.id, `Couldn't cancel. ${errorMessage(err)}`);
    }
  };

  const reconnect = async (e: ConnectEntryView, serverId: string) => {
    setError(e.id, null);
    try {
      await api.reconnectMcpServer(serverId);
    } catch (err) {
      setError(e.id, errorMessage(err));
    }
  };

  const remove = async (e: ConnectEntryView, serverId: string) => {
    try {
      await api.removeMcpServer(serverId);
      await load();
      onChanged();
    } catch (err) {
      setError(e.id, `Couldn't remove ${e.service}. ${errorMessage(err)}`);
    }
  };

  const showInToolServers = (st: McpServerStatus | undefined, e: ConnectEntryView) => {
    const name = st?.name ?? e.service;
    document.querySelector<HTMLButtonElement>(`button[aria-label="${CSS.escape(`More actions for ${name}`)}"]`)?.focus();
  };

  // Through the same native "Open link?" confirm as links in answers (`open_link`).
  const openPage = async (e: ConnectEntryView, url: string) => {
    try {
      await api.openLink(url);
    } catch (err) {
      setError(e.id, `Couldn't open the page. ${errorMessage(err)}`);
    }
  };

  const toggle = (id: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  const head = (
    <>
      <h2 id="connect-h">Connect</h2>
      {loadError === null && (
        <p className="dn-help">Services that run their own tool server, reviewed by Almend. Connecting adds it to your tool servers.</p>
      )}
    </>
  );

  if (loadError !== null) {
    return (
      <section className="card" aria-labelledby="connect-h" data-testid="connect-card">
        {head}
        <InlineMessage tone="error">{loadError} Servers you already added keep working under Tool servers.</InlineMessage>
      </section>
    );
  }
  if (view === null) {
    return (
      <section className="card" aria-labelledby="connect-h" aria-busy="true" data-testid="connect-card">
        {head}
        <div className="row">
          <div className="row-main">
            <Skeleton width="40%" />
            <Skeleton width="70%" />
          </div>
        </div>
        <div className="row">
          <div className="row-main">
            <Skeleton width="35%" />
            <Skeleton width="60%" />
          </div>
        </div>
      </section>
    );
  }

  return (
    <section className="card" aria-labelledby="connect-h" data-testid="connect-card">
      {head}
      {view.entries.map((e) => {
        const st = live.find((l) => l.server_id === e.server_id);
        const added = e.server_id !== null;
        const error = errors[e.id];
        const working = busy === e.id || st?.state === "signing_in" || st?.state === "connecting";
        const detailsId = `connect-details-${e.id}`;
        const details = (
          <Button
            variant="ghost"
            size="sm"
            aria-expanded={open.has(e.id)}
            aria-controls={open.has(e.id) ? detailsId : undefined}
            aria-label={`Details for ${e.service}`}
            icon={open.has(e.id) ? <ChevronDown {...ICON} aria-hidden="true" /> : <ChevronRight {...ICON} aria-hidden="true" />}
            onClick={() => toggle(e.id)}
          >
            Details
          </Button>
        );
        const show = (
          <Button variant="ghost" size="sm" aria-label={`Show ${e.service} in tool servers`} onClick={() => showInToolServers(st, e)}>
            Show in tool servers
          </Button>
        );
        const meta = `Run by ${e.vendor} · Sign in with ${e.vendor} · Reviewed ${formatDate(e.review_date)}`;

        let body: React.ReactNode;
        let actions: React.ReactNode;
        if (!e.reviewed) {
          body = (
            <>
              <span className="row-title">
                {e.service}{" "}
                <Badge tone="warning" icon={<TriangleAlert {...ICON} aria-hidden="true" />}>
                  No longer reviewed
                </Badge>
              </span>
              <span className="dn-help">Almend no longer reviews this server. It keeps working; remove it if you'd rather not use it.</span>
              {error !== undefined && <InlineMessage tone="error">{error}</InlineMessage>}
            </>
          );
          actions = (
            <>
              {show}
              <Button aria-label={`Remove ${e.service}`} onClick={() => e.server_id !== null && void remove(e, e.server_id)}>
                Remove
              </Button>
            </>
          );
        } else {
          let status: React.ReactNode = <span className="dn-help">{meta}</span>;
          if (error !== undefined) {
            status = <InlineMessage tone="error">{error}</InlineMessage>;
            actions = (
              <Button
                icon={<RotateCcw {...ICON} aria-hidden="true" />}
                aria-label={`Try connecting ${e.service} again`}
                onClick={() => void connect(e)}
              >
                Try again
              </Button>
            );
          } else if (working) {
            status = (
              <span className="dn-help connect-status">
                <ExternalLink {...ICON} aria-hidden="true" />
                {st?.state === "connecting" ? `Connecting to ${e.vendor}…` : `Finish signing in to ${e.vendor} in your browser.`}
              </span>
            );
            actions = (
              <>
                {added && (
                  <Button variant="ghost" size="sm" aria-label={`Cancel connecting ${e.service}`} onClick={() => void cancel(e)}>
                    Cancel
                  </Button>
                )}
                <Button loading aria-label={`Connecting ${e.service}`}>
                  Connecting…
                </Button>
              </>
            );
          } else if (added && st?.state === "connected") {
            actions = (
              <>
                <Badge tone="success" icon={<Check {...ICON} aria-hidden="true" />}>
                  Connected
                </Badge>
                {show}
              </>
            );
          } else if (added && st?.state === "needs_auth") {
            actions = (
              <>
                <Badge tone="warning" icon={<LogIn {...ICON} aria-hidden="true" />}>
                  Needs sign-in
                </Badge>
                <Button
                  icon={<LogIn {...ICON} aria-hidden="true" />}
                  aria-label={`Sign in to ${e.service}`}
                  onClick={() => e.server_id !== null && void signIn(e, e.server_id)}
                >
                  Sign in
                </Button>
              </>
            );
          } else if (added && st?.state === "failed") {
            status = <InlineMessage tone="error">{st.message ?? `${e.service} stopped.`}</InlineMessage>;
            actions = (
              <Button
                icon={<RotateCcw {...ICON} aria-hidden="true" />}
                aria-label={`Try connecting ${e.service} again`}
                onClick={() => e.server_id !== null && void reconnect(e, e.server_id)}
              >
                Try again
              </Button>
            );
          } else if (added) {
            actions = (
              <>
                <Badge>Added</Badge>
                {show}
              </>
            );
          } else {
            actions = (
              <>
                {details}
                <Button icon={<Plug {...ICON} aria-hidden="true" />} aria-label={`Connect ${e.service}`} onClick={() => void connect(e)}>
                  Connect
                </Button>
              </>
            );
          }
          body = (
            <>
              <span className="row-title">{e.service}</span>
              <span className="dn-help">{e.summary}</span>
              {status}
            </>
          );
        }
        return (
          <Fragment key={e.id}>
            <div className="row" data-testid="connect-row" data-entry={e.id}>
              <div className="row-main">{body}</div>
              <div className="row-actions mcp-actions">{actions}</div>
            </div>
            {open.has(e.id) && (
              <dl className="connect-details" id={detailsId}>
                <dt>Server</dt>
                <dd>
                  <code>{e.url}</code>
                </dd>
                <dt>Sign-in</dt>
                <dd>In your browser, with {e.vendor}. Almend holds no secret for this server.</dd>
                <dt>Your data</dt>
                <dd>{e.data}</dd>
                <dt>Asks for</dt>
                <dd>{e.scopes.length > 0 ? e.scopes.join(" · ") : `What ${e.vendor} asks for when you sign in`}</dd>
                <dt>Approvals</dt>
                <dd>
                  Reads ask once per chat; anything that creates, changes or deletes asks every time. You can change each tool
                  after connecting.
                </dd>
                <dt>Terms</dt>
                <dd>
                  <Button variant="ghost" size="sm" icon={<ExternalLink {...ICON} aria-hidden="true" />} onClick={() => void openPage(e, e.terms_url)}>
                    {e.vendor}'s terms
                  </Button>
                </dd>
                <dt>Reviewed</dt>
                <dd>
                  {formatDate(e.review_date)}, from{" "}
                  <Button variant="ghost" size="sm" icon={<ExternalLink {...ICON} aria-hidden="true" />} onClick={() => void openPage(e, e.review_source)}>
                    {e.vendor}'s tool server page
                  </Button>
                </dd>
              </dl>
            )}
          </Fragment>
        );
      })}
      <div className="dn-visually-hidden" role="status" aria-live="polite">
        {announce}
      </div>
    </section>
  );
}

function stateWords(s: string): string {
  switch (s) {
    case "connected":
      return "connected";
    case "needs_auth":
      return "needs sign-in";
    case "signing_in":
      return "signing in";
    case "connecting":
      return "connecting";
    default:
      return "couldn't connect";
  }
}

/** "2026-11-26" → "26 Nov 2026" (fixed English, like the rest of the UI). */
function formatDate(iso: string): string {
  const [y, m, d] = iso.split("-").map(Number);
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  if (y === undefined || m === undefined || d === undefined || months[m - 1] === undefined) return iso;
  return `${d} ${months[m - 1]} ${y}`;
}
