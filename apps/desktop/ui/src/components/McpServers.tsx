import { ChevronRight, LogIn, LogOut, MoreHorizontal, Pencil, Plus, RotateCcw, Trash2, X } from "lucide-react";
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type McpServerInput,
  type McpServerStatus,
  type McpServerView,
  type McpServersView,
  type McpValueView,
} from "../ipc/types";
import { DropdownMenu } from "./DropdownMenu";
import { McpToolsPanel } from "./McpTools";
import { Badge, Button, EmptyState, ICON, IconButton, InlineMessage, Skeleton, Switch, TextField } from "./primitives";
import { useToast } from "./toast";

/** How long Undo stays on the toast after a server is removed (the backend keeps it 15 s). */
const UNDO_MS = 10_000;

/**
 * Settings › Tools (#174) (T-SHELL-08, issue #49): add, edit and remove MCP servers,
 * sign in and out, and per-server status. Secret values are never shown: a saved secret reads
 * "Saved"; typing a new value replaces it.
 */
export function McpServersCard({ api, live }: { api: Api; live: McpServerStatus[] }) {
  const toast = useToast();
  const [view, setView] = useState<McpServersView | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ server: McpServerView | null } | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  // Servers whose Tools section is open (T-SHELL-11).
  const [openTools, setOpenTools] = useState<ReadonlySet<string>>(new Set());
  const toggleTools = (id: string) =>
    setOpenTools((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const addRef = useRef<HTMLButtonElement>(null);
  const cardRef = useRef<HTMLElement>(null);
  // Where focus goes when the form closes: the button that opened it (§7).
  const returnFocus = useRef<string | null>(null);

  const closeForm = () => {
    setEditing(null);
    const label = returnFocus.current;
    requestAnimationFrame(() => {
      const el =
        label === null
          ? addRef.current
          : [...(cardRef.current?.querySelectorAll<HTMLButtonElement>("button[aria-label]") ?? [])].find(
              (b) => b.getAttribute("aria-label") === label,
            );
      (el ?? addRef.current)?.focus();
    });
  };

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      setView(await api.listMcpServers());
    } catch (e) {
      setLoadError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  const status = (s: McpServerView): McpServerStatus | null =>
    live.find((l) => l.server_id === s.id) ?? s.status;

  // One live region for the card (not one per row, which reads every row out on load): it
  // says when a server's state changes after the list is shown.
  const [announce, setAnnounce] = useState("");
  const seen = useRef<Map<string, string> | null>(null);
  useEffect(() => {
    const now = new Map(live.map((l) => [l.server_id, l.state] as const));
    const before = seen.current;
    seen.current = now;
    if (before === null) return;
    const changed = live.filter((l) => before.has(l.server_id) && before.get(l.server_id) !== l.state);
    if (changed.length === 0) return;
    setAnnounce(changed.map((l) => `${l.name}: ${statusText(l)}`).join(". "));
  }, [live]);

  const run = async (key: string, what: string, f: () => Promise<McpServersView | null>) => {
    setBusy(key);
    try {
      const v = await f();
      if (v !== null) setView(v);
      else await load();
    } catch (e) {
      toast({ tone: "error", title: `${what} ${errorMessage(e)}` });
    } finally {
      setBusy(null);
    }
  };

  /** Removes at once; Undo on the toast puts it back (#189). Errors go to the menu's toast. */
  const remove = async (s: McpServerView) => {
    setView(await api.removeMcpServer(s.id));
    toast({
      tone: "info",
      title: `Removed “${s.name}”`,
      duration: UNDO_MS,
      action: {
        label: "Undo",
        onAction: () => {
          void api
            .restoreMcpServer(s.id)
            .then((v) => {
              setView(v);
              toast({ tone: "success", title: `Restored “${s.name}”` });
            })
            .catch((e: unknown) => toast({ tone: "error", title: `Couldn't restore ${s.name}. ${errorMessage(e)}` }));
        },
      },
    });
  };

  /** After a removal: the next row's actions, or Add tool server. */
  const focusAfterRemoving = (s: McpServerView) => (): HTMLElement | null => {
    const list = view?.servers ?? [];
    const next = list[list.findIndex((x) => x.id === s.id) + 1];
    const el =
      next === undefined
        ? null
        : (cardRef.current?.querySelector<HTMLButtonElement>(
            `button[aria-label="${CSS.escape(`More actions for ${next.name}`)}"]`,
          ) ?? null);
    return el ?? addRef.current;
  };

  if (editing !== null && view !== null) {
    return (
      <section className="card" aria-labelledby="mcp-form-h" ref={cardRef}>
        <ServerForm
          server={editing.server}
          localAllowed={view.local_servers_allowed}
          onCancel={closeForm}
          onSave={async (input) => {
            const v = await api.saveMcpServer(input, editing.server?.id ?? null);
            setView(v);
            closeForm();
            toast({ tone: "success", title: `${input.name ?? input.id} saved. Connecting…` });
          }}
        />
      </section>
    );
  }

  return (
    <section className="card" aria-labelledby="servers-h" ref={cardRef}>
      <div className="card-head">
        <h2 id="servers-h">Tool servers</h2>
        {view !== null && view.servers.length > 0 && (
          <Button
            ref={addRef}
            size="sm"
            variant="primary"
            icon={<Plus {...ICON} aria-hidden="true" />}
            onClick={() => {
              returnFocus.current = null;
              setEditing({ server: null });
            }}
          >
            Add tool server
          </Button>
        )}
      </div>
      <p className="dn-help">
        Tool servers (MCP) give models tools, such as reading files or searching an issue tracker. Read-only tools run
        without asking. Tools that change something ask for your approval first. Open a server's Tools to see what each
        tool does and choose when it asks.
      </p>
      {view === null && loadError === null && (
        <div aria-busy="true" aria-label="Loading tool servers">
          <Skeleton kind="block" />
        </div>
      )}
      {loadError !== null && (
        <InlineMessage
          tone="error"
          action={
            <Button size="sm" onClick={() => void load()}>
              Retry
            </Button>
          }
        >
          Couldn't load tool servers. {loadError}
        </InlineMessage>
      )}
      {view !== null && view.servers.length === 0 && (
        <EmptyState
          title="No tool servers yet"
          action={
            <Button
              ref={addRef}
              variant="primary"
              icon={<Plus {...ICON} aria-hidden="true" />}
              onClick={() => {
                returnFocus.current = null;
                setEditing({ server: null });
              }}
            >
              Add tool server
            </Button>
          }
        >
          Add a server to let models use its tools.
        </EmptyState>
      )}
      {view !== null &&
        [...view.servers]
          .sort((a, b) => a.name.localeCompare(b.name))
          .map((s) => {
            const st = status(s);
            const oauth = s.transport.type === "streamable_http" ? s.transport.oauth : null;
            const signedIn = oauth?.signed_in === true && st?.state !== "needs_auth";
            const toolsOpen = openTools.has(s.id);
            const toolsId = `mcp-tools-${s.id}`;
            return (
              <Fragment key={s.id}>
              <div className="row" data-testid="mcp-server-row">
                <div className="row-main">
                  <span className="row-title">{s.name}</span>
                  <span className="dn-help mcp-where">
                    {s.transport.type === "stdio"
                      ? [s.transport.command, ...s.transport.args].join(" ")
                      : s.transport.url}
                  </span>
                  <span className="dn-help">{statusText(st)}</span>
                </div>
                <div className="row-actions mcp-actions">
                  <StatusBadge status={st} />
                  {st?.state === "connected" && st.review > 0 && <Badge tone="warning">{st.review} to review</Badge>}
                  <Button
                    size="sm"
                    variant="ghost"
                    aria-expanded={toolsOpen}
                    aria-controls={toolsOpen ? toolsId : undefined}
                    aria-label={`Tools of ${s.name}`}
                    icon={<ChevronRight {...ICON} className="mcp-tools-chevron" aria-hidden="true" />}
                    onClick={() => toggleTools(s.id)}
                  >
                    Tools
                  </Button>
                  {oauth !== null && !signedIn && (
                    <Button
                      size="sm"
                      variant="secondary"
                      loading={busy === `signin:${s.id}` || st?.state === "signing_in"}
                      icon={<LogIn {...ICON} aria-hidden="true" />}
                      onClick={() => void run(`signin:${s.id}`, "Sign-in failed.", async () => (await api.signInMcpServer(s.id), null))}
                    >
                      Sign in
                    </Button>
                  )}
                  <DropdownMenu
                    label={`More actions for ${s.name}`}
                    trigger={<MoreHorizontal {...ICON} aria-hidden="true" />}
                    entries={[
                      ...(st?.state === "failed"
                        ? [
                            {
                              kind: "item" as const,
                              label: "Reconnect",
                              icon: <RotateCcw {...ICON} aria-hidden="true" />,
                              failure: "Couldn't reconnect",
                              run: () => run(`reconnect:${s.id}`, "Couldn't reconnect.", async () => (await api.reconnectMcpServer(s.id), null)),
                            },
                          ]
                        : []),
                      ...(oauth !== null && signedIn
                        ? [
                            {
                              kind: "item" as const,
                              label: "Sign out",
                              icon: <LogOut {...ICON} aria-hidden="true" />,
                              failure: "Couldn't sign out",
                              run: () =>
                                run(`signout:${s.id}`, "Couldn't sign out.", async () => {
                                  const v = await api.signOutMcpServer(s.id);
                                  toast({ tone: "info", title: `Signed out of ${s.name}` });
                                  return v;
                                }),
                            },
                          ]
                        : []),
                      {
                        kind: "item",
                        label: "Edit",
                        icon: <Pencil {...ICON} aria-hidden="true" />,
                        failure: "Couldn't open the server",
                        run: () => {
                          returnFocus.current = `More actions for ${s.name}`;
                          setEditing({ server: s });
                        },
                      },
                      { kind: "separator" },
                      {
                        kind: "item",
                        label: "Remove",
                        icon: <Trash2 {...ICON} aria-hidden="true" />,
                        danger: true,
                        failure: `Couldn't remove ${s.name}`,
                        run: () => remove(s),
                        focusAfter: focusAfterRemoving(s),
                      },
                    ]}
                  />
                </div>
              </div>
              {toolsOpen && (
                <div id={toolsId}>
                  <McpToolsPanel
                    api={api}
                    serverId={s.id}
                    serverName={s.name}
                    version={`${st?.state ?? "none"}:${st?.tools ?? 0}:${st?.review ?? 0}:${st?.revision ?? 0}`}
                  />
                </div>
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


function statusText(st: McpServerStatus | null): string {
  switch (st?.state) {
    case undefined:
      return "Not connected yet.";
    case "connected":
      return `${st.tools} ${st.tools === 1 ? "tool" : "tools"}`;
    case "connecting":
      return "Connecting…";
    case "signing_in":
      return st.message ?? "Finish signing in in your browser.";
    case "needs_auth":
      return st.message ?? "Sign in to use this server's tools.";
    case "failed":
      return st.message ?? "Couldn't connect.";
  }
}

function StatusBadge({ status }: { status: McpServerStatus | null }) {
  switch (status?.state) {
    case "connected":
      return <Badge tone="success">Connected</Badge>;
    case "failed":
      return <Badge tone="danger">Failed</Badge>;
    case "needs_auth":
      return <Badge tone="warning">Needs sign-in</Badge>;
    case "signing_in":
      return <Badge tone="neutral">Signing in</Badge>;
    case "connecting":
      return <Badge tone="neutral">Connecting</Badge>;
    default:
      return <Badge tone="neutral">Not connected</Badge>;
  }
}

// ---- form

interface ValueRow {
  key: number;
  name: string;
  secret: boolean;
  value: string;
  /** A secret is stored; an empty value keeps it. */
  stored: boolean;
}

let rowKey = 0;
const toRows = (v: McpValueView[]): ValueRow[] =>
  v.map((x) => ({ key: ++rowKey, name: x.name, secret: x.secret, value: x.value ?? "", stored: x.stored }));

/** Names that should hold a secret (mirrors dn-tools' rule, which refuses plain values). */
const looksSecret = (name: string) =>
  /KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTHORIZATION|COOKIE|SESSION|PRIVATE|DATABASE_URL|CONNECTION_STRING|(^|[^A-Z0-9])(O?AUTH|BASICAUTH|PAT|DSN)([^A-Z0-9]|$)/i.test(
    name.replace(/-/g, "_"),
  );

function ServerForm({
  server,
  localAllowed,
  onSave,
  onCancel,
}: {
  server: McpServerView | null;
  localAllowed: boolean;
  onSave: (input: McpServerInput) => Promise<void>;
  onCancel: () => void;
}) {
  const t = server?.transport;
  const [kind, setKind] = useState<"stdio" | "streamable_http">(
    t?.type ?? (localAllowed ? "stdio" : "streamable_http"),
  );
  const [name, setName] = useState(server?.name ?? "");
  const [id, setId] = useState(server?.id ?? "");
  const [command, setCommand] = useState(t?.type === "stdio" ? t.command : "");
  const [args, setArgs] = useState(t?.type === "stdio" ? t.args.join("\n") : "");
  const [env, setEnv] = useState<ValueRow[]>(t?.type === "stdio" ? toRows(t.env) : []);
  const [url, setUrl] = useState(t?.type === "streamable_http" ? t.url : "");
  const [headers, setHeaders] = useState<ValueRow[]>(t?.type === "streamable_http" ? toRows(t.headers) : []);
  const [oauth, setOauth] = useState(t?.type === "streamable_http" && t.oauth !== null);
  const [scopes, setScopes] = useState(t?.type === "streamable_http" ? (t.oauth?.scopes.join(" ") ?? "") : "");
  const [clientId, setClientId] = useState(t?.type === "streamable_http" ? (t.oauth?.client_id ?? "") : "");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const initial = useRef<string | null>(null);
  const snapshot = JSON.stringify([kind, name, id, command, args, env, url, headers, oauth, scopes, clientId]);
  if (initial.current === null) initial.current = snapshot;
  const dirty = snapshot !== initial.current;
  // Saved secrets aren't reused for a new address or program (including new arguments:
  // `npx pkg-a` vs `npx pkg-b`): they must be typed again.
  const argList = args
    .split("\n")
    .map((a) => a.trim())
    .filter((a) => a !== "");
  const moved =
    t !== undefined &&
    (t.type === "stdio"
      ? command.trim() !== t.command || JSON.stringify(argList) !== JSON.stringify(t.args)
      : url.trim() !== t.url);

  useEffect(() => {
    headingRef.current?.focus();
  }, []);

  // Suggest an id from the name while adding.
  const suggestedId = name
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);

  const values = (rows: ValueRow[]) =>
    rows
      .filter((r) => r.name.trim() !== "" || r.value !== "")
      .map((r) => ({ name: r.name.trim(), secret: r.secret, value: r.secret && r.value === "" ? null : r.value }));

  const submit = async () => {
    setError(null);
    const finalId = server?.id ?? (id.trim() || suggestedId);
    if (finalId === "") {
      setError("Give the server a name.");
      return;
    }
    const input: McpServerInput = {
      id: finalId,
      name: name.trim() === "" ? null : name.trim(),
      transport:
        kind === "stdio"
          ? {
              type: "stdio",
              command: command.trim(),
              args: argList,
              env: values(env),
            }
          : {
              type: "streamable_http",
              url: url.trim(),
              headers: values(headers),
              oauth,
              scopes: scopes.split(/[\s,]+/).filter((s) => s !== ""),
              client_id: clientId.trim() === "" ? null : clientId.trim(),
            },
    };
    setSaving(true);
    try {
      await onSave(input);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      className="mcp-form"
      aria-labelledby="mcp-form-h"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <h2 id="mcp-form-h" ref={headingRef} tabIndex={-1}>
        {server === null ? "Add a tool server" : `Edit ${server.name}`}
      </h2>
      <TextField label="Name" value={name} placeholder="e.g. Work files" onChange={(e) => setName(e.target.value)} />
      {server === null ? (
        <TextField
          label="ID"
          value={id}
          placeholder={suggestedId === "" ? "e.g. work-files" : suggestedId}
          help="Lowercase letters, digits, - and _. Tools are named after it; it can't change later."
          spellCheck={false}
          onChange={(e) => setId(e.target.value)}
        />
      ) : (
        <TextField label="ID" value={server.id} readOnly help="The ID can't change." />
      )}

      <fieldset className="mcp-kind">
        <legend>Where it runs</legend>
        {localAllowed && (
          <label>
            <input type="radio" name="mcp-kind" checked={kind === "stdio"} onChange={() => setKind("stdio")} disabled={server !== null} />
            On this computer (a program Demios starts)
          </label>
        )}
        <label>
          <input
            type="radio"
            name="mcp-kind"
            checked={kind === "streamable_http"}
            onChange={() => setKind("streamable_http")}
            disabled={server !== null}
          />
          Remote (an HTTPS address)
        </label>
        {!localAllowed && (
          <p className="dn-help">The Flatpak version of Demios runs in a sandbox and can't start programs on your computer, so only remote (HTTPS) servers work.</p>
        )}
      </fieldset>

      {kind === "stdio" ? (
        <>
          <TextField
            label="Command"
            value={command}
            placeholder="e.g. /usr/local/bin/mcp-server-files"
            help="The program Demios starts. It runs with your permissions, so add only programs you trust."
            spellCheck={false}
            onChange={(e) => setCommand(e.target.value)}
          />
          <div className="dn-field">
            <label htmlFor="mcp-args">Arguments</label>
            <textarea
              id="mcp-args"
              className="mcp-textarea"
              rows={3}
              value={args}
              spellCheck={false}
              aria-describedby="mcp-args-help"
              onChange={(e) => setArgs(e.target.value)}
            />
            <span id="mcp-args-help" className="dn-field-help">
              One per line.
            </span>
          </div>
          <ValueRows label="Environment variables" what="variable" rows={env} onChange={setEnv} moved={moved} />
        </>
      ) : (
        <>
          <TextField
            label="Server address"
            type="url"
            value={url}
            placeholder="https://example.com/mcp"
            spellCheck={false}
            onChange={(e) => setUrl(e.target.value)}
          />
          <ValueRows label="Headers" what="header" rows={headers} onChange={setHeaders} moved={moved} />
          <Switch
            label="Sign in with the server's account (OAuth)"
            description="After saving, use Sign in to open the server's sign-in page in your browser."
            checked={oauth}
            onCheckedChange={setOauth}
          />
          {oauth && (
            <>
              <TextField label="Scopes (optional)" value={scopes} placeholder="e.g. read write" onChange={(e) => setScopes(e.target.value)} />
              <TextField
                label="Client ID (optional)"
                value={clientId}
                help="Only if the server gave you one; otherwise Demios registers itself."
                spellCheck={false}
                onChange={(e) => setClientId(e.target.value)}
              />
            </>
          )}
        </>
      )}

      {error !== null && <InlineMessage tone="error">{error}</InlineMessage>}
      {confirmDiscard && (
        <InlineMessage
          tone="warning"
          action={
            <>
              <Button size="sm" variant="danger" onClick={onCancel}>
                Discard changes
              </Button>
              <Button size="sm" onClick={() => setConfirmDiscard(false)}>
                Keep editing
              </Button>
            </>
          }
        >
          You have unsaved changes.
        </InlineMessage>
      )}
      <div className="mcp-form-actions">
        <Button variant="ghost" onClick={() => (dirty ? setConfirmDiscard(true) : onCancel())}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" loading={saving}>
          {server === null ? "Add tool server" : "Save changes"}
        </Button>
      </div>
    </form>
  );
}

function ValueRows({
  label,
  what,
  rows,
  onChange,
  moved = false,
}: {
  label: string;
  what: string;
  rows: ValueRow[];
  onChange: (rows: ValueRow[]) => void;
  /** The address or program changed: saved secrets must be typed again. */
  moved?: boolean;
}) {
  const set = (key: number, patch: Partial<ValueRow>) =>
    onChange(
      rows.map((r) => {
        if (r.key !== key) return r;
        const next = { ...r, ...patch };
        // A secret-looking name becomes a secret (plain values are refused for it).
        if (patch.name !== undefined && !r.secret && looksSecret(next.name)) next.secret = true;
        return next;
      }),
    );
  return (
    <fieldset className="mcp-values">
      <legend>{label}</legend>
      {rows.length === 0 && <p className="dn-help">None.</p>}
      {rows.map((r) => (
        <div className="mcp-value-row" key={r.key}>
          <TextField
            label={`Name of ${what}`}
            value={r.name}
            spellCheck={false}
            onChange={(e) => set(r.key, { name: e.target.value })}
          />
          <TextField
            label={r.secret ? "Secret value" : "Value"}
            type={r.secret ? "password" : "text"}
            autoComplete="off"
            spellCheck={false}
            value={r.value}
            placeholder={r.secret && r.stored ? (moved ? "Type it again" : "Saved") : ""}
            onChange={(e) => set(r.key, { value: e.target.value })}
          />
          <label className="mcp-secret">
            <input
              type="checkbox"
              checked={r.secret}
              disabled={looksSecret(r.name)}
              onChange={(e) => set(r.key, { secret: e.target.checked, value: "" })}
            />
            Secret
          </label>
          <IconButton size="sm" label={`Remove ${what} ${r.name || ""}`.trim()} onClick={() => onChange(rows.filter((x) => x.key !== r.key))}>
            <X {...ICON} aria-hidden="true" />
          </IconButton>
        </div>
      ))}
      <Button
        size="sm"
        variant="ghost"
        icon={<Plus {...ICON} aria-hidden="true" />}
        onClick={() => onChange([...rows, { key: ++rowKey, name: "", secret: false, value: "", stored: false }])}
      >
        Add {what}
      </Button>
      <p className="dn-help">
        {moved
          ? "The address or program changed, so saved secrets aren't reused: type them again."
          : "Secret values are kept in your system keychain and are never shown again. Leave a saved one empty to keep it."}
      </p>
    </fieldset>
  );
}
