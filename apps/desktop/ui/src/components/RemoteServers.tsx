import { Plus, RefreshCw, Trash2, Wrench } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type ModelCatalog,
  type RemoteModelView,
  type RemoteServerStatus,
  type RemoteServerView,
} from "../ipc/types";
import { Badge, Button, EmptyState, ICON, InlineMessage, Switch, TextField } from "./primitives";
import { useToast } from "./toast";

/** How long Undo stays on the toast after a server is removed (the backend keeps it 15 s). */
const UNDO_MS = 10_000;

/**
 * Settings › Models › Model servers (T-PRV-05, issue #113): OpenAI-compatible servers such
 * as Ollama, LM Studio, llama.cpp or vLLM, added by address with an optional key.
 * - Each server shows whether it answers now: Connected, Needs key, or Unreachable with Retry.
 * - Each model shows whether it makes tool calls, as the server reports it. When the server
 *   doesn't say, the user can turn tool calls on; nothing is assumed.
 * - Remove takes effect at once, with Undo for 10 seconds.
 */
export function RemoteServersCard({
  api,
  catalog,
  onCatalog,
}: {
  api: Api;
  catalog: ModelCatalog;
  onCatalog: (c: ModelCatalog) => void;
}) {
  const toast = useToast();
  const [busy, setBusy] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [status, setStatus] = useState<Record<string, RemoteServerStatus | "checking">>({});
  const formId = useId();
  const addRef = useRef<HTMLButtonElement>(null);
  const servers = catalog.remote_servers;

  const check = useCallback(
    async (id: string) => {
      setStatus((s) => ({ ...s, [id]: "checking" }));
      try {
        const st = await api.checkRemoteServer(id);
        setStatus((s) => ({ ...s, [id]: st }));
      } catch (e) {
        setStatus((s) => ({ ...s, [id]: { state: "unreachable", message: errorMessage(e) } }));
      }
    },
    [api],
  );

  // Check each server once when it appears (and again after Refresh or Retry).
  const ids = servers.map((s) => s.id).join(",");
  useEffect(() => {
    for (const id of ids.split(",").filter(Boolean)) {
      if (status[id] === undefined) void check(id);
    }
    // `status` is read only to skip servers already checked, so it isn't a dependency.
  }, [ids, check]);

  const refresh = async (s: RemoteServerView) => {
    setBusy(`refresh:${s.id}`);
    try {
      onCatalog(await api.refreshRemoteServer(s.id));
      toast({ tone: "success", title: `${s.name}: model list updated` });
      void check(s.id);
    } catch (e) {
      toast({ tone: "error", title: `Couldn't refresh ${s.name}. ${errorMessage(e)}` });
    } finally {
      setBusy(null);
    }
  };

  const remove = async (s: RemoteServerView) => {
    setBusy(`remove:${s.id}`);
    try {
      onCatalog(await api.removeRemoteServer(s.id));
      toast({
        tone: "info",
        title: `${s.name} removed`,
        duration: UNDO_MS,
        action: {
          label: "Undo",
          onAction: () => {
            void api
              .restoreRemoteServer(s.id)
              .then((c) => {
                onCatalog(c);
                toast({ tone: "success", title: `${s.name} restored` });
              })
              .catch((e: unknown) => toast({ tone: "error", title: errorMessage(e) }));
          },
        },
      });
    } catch (e) {
      toast({ tone: "error", title: `Couldn't remove ${s.name}. ${errorMessage(e)}` });
    } finally {
      setBusy(null);
    }
  };

  const setTools = async (s: RemoteServerView, m: RemoteModelView, enabled: boolean) => {
    try {
      onCatalog(await api.setRemoteModelTools(s.id, m.id, enabled));
    } catch (e) {
      toast({ tone: "error", title: errorMessage(e) });
    }
  };

  const addButton = (
    <Button
      ref={addRef}
      variant="primary"
      icon={<Plus {...ICON} aria-hidden="true" />}
      aria-expanded={adding}
      aria-controls={formId}
      data-testid="add-model-server"
      onClick={() => setAdding((a) => !a)}
    >
      Add model server
    </Button>
  );

  return (
    <section className="card" aria-labelledby="remote-h" data-testid="remote-servers">
      <h2 id="remote-h">Model servers</h2>
      <p className="dn-help">
        Ollama, LM Studio, llama.cpp, vLLM or any server with an OpenAI-compatible API. Servers on this computer or your
        local network can use http://; any other server needs https://. Messages in chats that use a server's models are sent
        to that server.
      </p>
      {servers.length === 0 && !adding ? (
        <EmptyState title="No model servers yet" action={addButton}>
          Add the address of a server you run, such as http://localhost:11434 for Ollama.
        </EmptyState>
      ) : (
        <div className="row-actions">{addButton}</div>
      )}
      {adding && (
        <AddServerForm
          id={formId}
          api={api}
          onCatalog={onCatalog}
          onDone={() => {
            setAdding(false);
            addRef.current?.focus();
          }}
        />
      )}
      {servers.map((s) => (
        <div key={s.id} data-testid="remote-server">
          <div className="row">
            <div className="row-main">
              <span className="row-title">{s.name}</span>
              <span className="dn-help">
                {s.base_url} · {s.models.length === 1 ? "1 model" : `${s.models.length} models`}
              </span>
            </div>
            <StatusBadge status={status[s.id]} hasKey={s.has_key} onRetry={() => void check(s.id)} />
            <div className="row-actions">
              <Button
                variant="secondary"
                loading={busy === `refresh:${s.id}`}
                icon={<RefreshCw {...ICON} aria-hidden="true" />}
                aria-label={`Refresh ${s.name}`}
                onClick={() => void refresh(s)}
              >
                Refresh
              </Button>
              <Button
                variant="ghost"
                loading={busy === `remove:${s.id}`}
                icon={<Trash2 {...ICON} aria-hidden="true" />}
                aria-label={`Remove ${s.name}`}
                onClick={() => void remove(s)}
              >
                Remove
              </Button>
            </div>
          </div>
          {s.models.length === 0 && (
            <InlineMessage tone="info">
              The server has no models yet. Load or download one there, then choose Refresh.
            </InlineMessage>
          )}
          {s.models.map((m) => (
            <ModelRow key={m.id} model={m} onTools={(on) => void setTools(s, m, on)} />
          ))}
        </div>
      ))}
    </section>
  );
}

function StatusBadge({
  status,
  hasKey,
  onRetry,
}: {
  status: RemoteServerStatus | "checking" | undefined;
  hasKey: boolean;
  onRetry: () => void;
}) {
  if (status === undefined || status === "checking") {
    return <Badge tone="neutral">Checking…</Badge>;
  }
  if (status.state === "connected") {
    return <Badge tone="success">Connected</Badge>;
  }
  if (status.state === "needs_key") {
    return (
      <span className="row-actions" title={status.message}>
        <Badge tone="warning">{hasKey ? "Key refused" : "Needs key"}</Badge>
      </span>
    );
  }
  return (
    <span className="row-actions" title={status.message}>
      <Badge tone="danger">Unreachable</Badge>
      <Button size="sm" variant="ghost" onClick={onRetry}>
        Retry
      </Button>
    </span>
  );
}

function ModelRow({ model: m, onTools }: { model: RemoteModelView; onTools: (on: boolean) => void }) {
  if (!m.embedding && !m.tool_support_reported) {
    return (
      <div className="row" data-testid="remote-model" data-model={m.id}>
        <div className="row-main">
          <Switch
            label={`${m.id}: tool calls`}
            description="The server doesn't say whether this model makes tool calls. Turn this on only if it does: without them it can't make artifacts or use tools."
            checked={m.tool_support === "supported"}
            onCheckedChange={onTools}
          />
        </div>
      </div>
    );
  }
  return (
    <div className="row" data-testid="remote-model" data-model={m.id}>
      <div className="row-main">
        <span className="row-title">{m.id}</span>
      </div>
      {m.embedding ? (
        <Badge tone="neutral">Embeddings</Badge>
      ) : m.tool_support === "supported" ? (
        <Badge tone="success" icon={<Wrench size={12} strokeWidth={1.5} aria-hidden="true" />}>
          Tool calls
        </Badge>
      ) : (
        <Badge tone="neutral">No tool calls</Badge>
      )}
    </div>
  );
}

function AddServerForm({
  id,
  api,
  onCatalog,
  onDone,
}: {
  id: string;
  api: Api;
  onCatalog: (c: ModelCatalog) => void;
  onDone: () => void;
}) {
  const toast = useToast();
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const add = async () => {
    if (url.trim() === "") {
      setError("Enter the server's address, for example http://localhost:11434.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const c = await api.addRemoteServer(name, url, key.trim() === "" ? null : key);
      onCatalog(c);
      const added = c.remote_servers.at(-1);
      const n = added?.models.length ?? 0;
      toast({
        tone: "success",
        title: `${added?.name ?? "Server"} added: ${n === 0 ? "no models yet" : n === 1 ? "1 model" : `${n} models`}`,
      });
      onDone();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      id={id}
      className="mcp-form"
      aria-label="Add model server"
      data-testid="add-server-form"
      onSubmit={(e) => {
        e.preventDefault();
        void add();
      }}
    >
      <TextField
        label="Address"
        type="url"
        value={url}
        placeholder="http://localhost:11434"
        spellCheck={false}
        autoComplete="off"
        autoFocus
        data-testid="server-url"
        error={error}
        help="Ollama: http://localhost:11434 · LM Studio: http://localhost:1234 · /v1 is added if the address has no path."
        onChange={(e) => {
          setUrl(e.target.value);
          if (error !== null) setError(null);
        }}
      />
      <TextField
        label="Name (optional)"
        value={name}
        placeholder="e.g. Ollama on my Mac"
        data-testid="server-name"
        onChange={(e) => setName(e.target.value)}
      />
      <TextField
        label="API key (optional)"
        type="password"
        value={key}
        autoComplete="off"
        spellCheck={false}
        placeholder="Only if the server needs one"
        help="Saved in your system keychain."
        data-testid="server-key"
        onChange={(e) => setKey(e.target.value)}
      />
      <div className="mcp-form-actions">
        <Button variant="ghost" onClick={onDone}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" loading={busy} data-testid="add-server">
          Add
        </Button>
      </div>
    </form>
  );
}
