import { Plus, RefreshCw, Server, Trash2, Wrench } from "lucide-react";
import { useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import { errorMessage, type ModelCatalog, type RemoteModelView, type RemoteServerView } from "../ipc/types";
import { ConfirmDialog } from "./ConfirmDialog";
import { Badge, Button, ICON, InlineMessage, Switch, TextField } from "./primitives";
import { useToast } from "./toast";

/**
 * Settings › Models › Model servers (T-PRV-05, issue #113): OpenAI-compatible servers such
 * as Ollama, LM Studio, llama.cpp or vLLM, added by address with an optional key. Each
 * model shows whether it makes tool calls, as the server reports it. When the server doesn't
 * say, the user can turn tool calls on; nothing is assumed.
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
  const [removing, setRemoving] = useState<RemoteServerView | null>(null);
  const [removeError, setRemoveError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const removeTrigger = useRef<HTMLButtonElement | null>(null);

  const refresh = async (s: RemoteServerView) => {
    setBusy(`refresh:${s.id}`);
    try {
      onCatalog(await api.refreshRemoteServer(s.id));
      toast({ tone: "success", title: `${s.name}: model list updated` });
    } catch (e) {
      toast({ tone: "error", title: `Couldn't refresh ${s.name}. ${errorMessage(e)}` });
    } finally {
      setBusy(null);
    }
  };

  const remove = async () => {
    if (removing === null) return;
    setBusy(`remove:${removing.id}`);
    setRemoveError(null);
    try {
      onCatalog(await api.removeRemoteServer(removing.id));
      toast({ tone: "info", title: `${removing.name} removed` });
      setRemoving(null);
    } catch (e) {
      setRemoveError(errorMessage(e));
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

  const servers = catalog.remote_servers;
  return (
    <section className="card" aria-labelledby="remote-h" data-testid="remote-servers">
      <h2 id="remote-h">Model servers</h2>
      <p className="dn-help">
        Ollama, LM Studio, llama.cpp, vLLM or any server with an OpenAI-compatible API. Servers on this computer or your
        local network can use http://; any other server needs https://. Messages in chats that use a server's models are sent
        to that server.
      </p>
      {servers.map((s) => (
        <div key={s.id} data-testid="remote-server">
          <div className="row">
            <div className="row-main">
              <span className="row-title">{s.name}</span>
              <span className="dn-help">
                {s.base_url} · {s.has_key ? "key saved in your system keychain" : "no key"} ·{" "}
                {s.models.length === 1 ? "1 model" : `${s.models.length} models`}
              </span>
            </div>
            <div className="row-actions">
              <Button
                variant="secondary"
                loading={busy === `refresh:${s.id}`}
                icon={<RefreshCw {...ICON} aria-hidden="true" />}
                onClick={() => void refresh(s)}
              >
                Refresh
              </Button>
              <Button
                variant="ghost"
                icon={<Trash2 {...ICON} aria-hidden="true" />}
                aria-label={`Remove ${s.name}`}
                onClick={(e) => {
                  removeTrigger.current = e.currentTarget;
                  setRemoveError(null);
                  setRemoving(s);
                }}
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
      <AddServerForm api={api} onCatalog={onCatalog} />
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => {
          if (!o && busy === null) setRemoving(null);
        }}
        title={`Remove ${removing?.name ?? "server"}?`}
        confirmLabel="Remove server"
        busy={busy?.startsWith("remove:") === true}
        error={removeError}
        onConfirm={() => void remove()}
        returnFocusTo={removeTrigger}
      >
        <p>This deletes, on this computer:</p>
        <ul>
          <li>the server's address and model list</li>
          {removing?.has_key === true && <li>its key in your system keychain</li>}
          <li>any task set to one of its models (choose another model for it)</li>
        </ul>
        <p>Nothing on the server itself changes. Chats that used its models keep their history.</p>
      </ConfirmDialog>
    </section>
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

function AddServerForm({ api, onCatalog }: { api: Api; onCatalog: (c: ModelCatalog) => void }) {
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
      setName("");
      setUrl("");
      setKey("");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className="mcp-form"
      aria-labelledby="add-server-h"
      data-testid="add-server-form"
      onSubmit={(e) => {
        e.preventDefault();
        void add();
      }}
    >
      <h3 id="add-server-h" className="card-subhead">
        <Server {...ICON} aria-hidden="true" /> Add a server
      </h3>
      <TextField
        label="Address"
        type="url"
        value={url}
        placeholder="http://localhost:11434"
        spellCheck={false}
        autoComplete="off"
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
        help="Saved in your system keychain and sent only to this server."
        data-testid="server-key"
        onChange={(e) => setKey(e.target.value)}
      />
      <div className="row-actions">
        <Button type="submit" variant="secondary" loading={busy} icon={<Plus {...ICON} aria-hidden="true" />} data-testid="add-server">
          Add server
        </Button>
      </div>
    </form>
  );
}
