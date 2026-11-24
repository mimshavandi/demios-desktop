import { Cpu, KeyRound, Trash2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type AppInfo,
  type LocalDataSummary,
  type McpServerStatus,
  type ModelCatalog,
  type ProviderInfo,
  type Settings,
  type SlotInfo,
} from "../ipc/types";
import { Badge, Button, ICON, InlineMessage, Select, Skeleton, TextField, type SelectGroup } from "./primitives";
import { ConfirmDialog } from "./ConfirmDialog";
import { LocalModelsCard, formatBytes } from "./LocalModels";
import { useToast } from "./toast";

export type SettingsSection = "models" | "general";

interface Props {
  api: Api;
  info: AppInfo | null;
  section: SettingsSection;
  onSection: (s: SettingsSection) => void;
  /** Keeps the shell's copy of the catalog (model badge, composer state) current. */
  onCatalog: (c: ModelCatalog) => void;
  /** MCP tool servers and their connection state. */
  servers: McpServerStatus[];
}

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; catalog: ModelCatalog };

export function SettingsView({ api, info, section, onSection, onCatalog, servers }: Props) {
  const toast = useToast();
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  const headingRef = useRef<HTMLHeadingElement>(null);

  // ⌘, and the sidebar land on the same page; focus its heading (design-system §7).
  useEffect(() => {
    headingRef.current?.focus();
  }, [section]);

  const set = useCallback(
    (catalog: ModelCatalog) => {
      setLoad({ kind: "ready", catalog });
      onCatalog(catalog);
    },
    [onCatalog],
  );

  const fetch = useCallback(async () => {
    setLoad({ kind: "loading" });
    try {
      set(await api.listModels());
    } catch (e) {
      setLoad({ kind: "error", message: errorMessage(e) });
    }
  }, [api, set]);

  useEffect(() => {
    void fetch();
  }, [fetch]);

  const save = async (catalog: ModelCatalog, next: Settings, what: string) => {
    try {
      const settings = await api.updateSettings(next);
      set({ ...catalog, settings });
      toast({ tone: "success", title: `${what} saved` });
    } catch (e) {
      toast({ tone: "error", title: `Couldn't save ${what.toLowerCase()}. ${errorMessage(e)}` });
    }
  };

  return (
    <section className="settings" aria-labelledby="settings-h">
      <div className="settings-column">
        <h1 id="settings-h" ref={headingRef} tabIndex={-1}>
          Settings
        </h1>
        <div
          className="tabs"
          role="tablist"
          aria-label="Settings sections"
          onKeyDown={(e) => {
            if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
            e.preventDefault();
            const next = section === "models" ? "general" : "models";
            onSection(next);
            e.currentTarget.querySelector<HTMLButtonElement>(`[data-section="${next}"]`)?.focus();
          }}
        >
          {(["models", "general"] as const).map((s) => (
            <button
              key={s}
              type="button"
              role="tab"
              id={`tab-${s}`}
              aria-controls={`panel-${s}`}
              aria-selected={section === s}
              tabIndex={section === s ? 0 : -1}
              data-section={s}
              className={section === s ? "tab selected" : "tab"}
              onClick={() => onSection(s)}
            >
              {s === "models" ? "Models" : "General"}
            </button>
          ))}
        </div>

        <div role="tabpanel" id={`panel-${section}`} aria-labelledby={`tab-${section}`} className="tabpanel">
        {load.kind === "loading" && (
          <div className="card" aria-busy="true" aria-label="Loading settings">
            <Skeleton width="30%" />
            <Skeleton kind="block" />
            <Skeleton kind="block" />
          </div>
        )}
        {load.kind === "error" && (
          <InlineMessage
            tone="error"
            action={
              <Button size="sm" onClick={() => void fetch()}>
                Retry
              </Button>
            }
          >
            Couldn't load settings. {load.message}
          </InlineMessage>
        )}
        {load.kind === "ready" && section === "models" && (
          <ModelsPage
            api={api}
            info={info}
            servers={servers}
            catalog={load.catalog}
            onCatalog={set}
            onSave={(next, what) => void save(load.catalog, next, what)}
          />
        )}
        {load.kind === "ready" && section === "general" && (
          <GeneralPage api={api} info={info} />
        )}
        </div>
      </div>
    </section>
  );
}

function ModelsPage({
  api,
  info,
  servers,
  catalog,
  onCatalog,
  onSave,
}: {
  api: Api;
  info: AppInfo | null;
  servers: McpServerStatus[];
  catalog: ModelCatalog;
  onCatalog: (c: ModelCatalog) => void;
  onSave: (s: Settings, what: string) => void;
}) {
  const s = catalog.settings;
  // A download finished: pickers and the composer should see the new model.
  const refreshCatalog = useCallback(() => {
    void api
      .listModels()
      .then(onCatalog)
      .catch(() => {});
  }, [api, onCatalog]);
  return (
    <>
      <section className="card" aria-labelledby="slots-h">
        <h2 id="slots-h">Model for each task</h2>
        <p className="dn-help">A task without a model shows a message instead of quietly using another one.</p>
        {catalog.slots.map((slot) => (
          <SlotRow
            key={slot.slot_id}
            slot={slot}
            catalog={catalog}
            devFake={info?.dev_fake_model ?? false}
            onChange={(model) => {
              const slots = { ...s.slots };
              if (model === "") delete slots[slot.slot_id];
              else slots[slot.slot_id] = model;
              onSave({ ...s, slots }, `${slot.label} model`);
            }}
          />
        ))}
      </section>

      {info?.dev_fake_model === true && (
        <section className="card" aria-labelledby="dev-h">
          <h2 id="dev-h">Development</h2>
          <div className="row">
            <div className="row-main">
              <span className="row-title">Dev fake model</span>
              <span className="dn-help">
                Streams a canned reply so the app works without a real model. Development builds only.
              </span>
            </div>
            <Badge tone="local" icon={<Cpu size={12} strokeWidth={1.5} aria-hidden="true" />}>
              Ready
            </Badge>
          </div>
        </section>
      )}

      <LocalModelsCard api={api} initial={catalog.local_models} onReady={refreshCatalog} />

      {servers.length > 0 && (
        <section className="card" aria-labelledby="servers-h">
          <h2 id="servers-h">Tool servers</h2>
          {[...servers]
            .sort((a, b) => a.name.localeCompare(b.name))
            .map((s) => (
              <div className="row" key={s.server_id} data-testid="mcp-server-row">
                <div className="row-main">
                  <span className="row-title">{s.name}</span>
                  <span className="dn-help">
                    {s.state === "connected"
                      ? `${s.tools} ${s.tools === 1 ? "tool" : "tools"}`
                      : s.state === "connecting"
                        ? "Connecting…"
                        : (s.message ?? "Couldn't connect.")}
                  </span>
                </div>
                <Badge tone={s.state === "connected" ? "success" : s.state === "failed" ? "danger" : "neutral"}>
                  {s.state === "connected" ? "Connected" : s.state === "failed" ? "Failed" : "Connecting"}
                </Badge>
              </div>
            ))}
        </section>
      )}

      <section className="card" aria-labelledby="keys-h">
        <h2 id="keys-h">Provider keys</h2>
        <p className="dn-help">
          Bring your own key. Keys are never shown again after you save them. Storage: {catalog.key_store}.
        </p>
        {catalog.providers.map((p) => (
          <ProviderRow key={p.id} api={api} provider={p} onCatalog={onCatalog} />
        ))}
      </section>
    </>
  );
}

function SlotRow({
  slot,
  catalog,
  devFake,
  onChange,
}: {
  slot: SlotInfo;
  catalog: ModelCatalog;
  devFake: boolean;
  onChange: (model: string) => void;
}) {
  const current = catalog.settings.slots[slot.slot_id] ?? "";
  const usable = catalog.models.filter((m) => m.capabilities.includes(slot.capability));
  const groups: SelectGroup[] = [
    {
      label: "On this device",
      options: usable
        .filter((m) => m.source === "local" || m.source === "dev")
        .map((m) => ({ value: m.model_ref, label: m.label })),
    },
    ...catalog.providers.map((p) => ({
      label: p.name,
      options: usable
        .filter((m) => m.model_ref.startsWith(`byok:${p.id}:`))
        .map((m) => ({
          value: m.model_ref,
          label: m.label.replace(`${p.name} · `, ""),
          disabled: !m.available,
          ...(m.available ? {} : { hint: "Not in this build yet" }),
        })),
    })),
  ];
  const id = `slot-${slot.slot_id}`;
  const auto = slot.slot_id === "chat" ? catalog.chat_model : null;
  const autoLabel = auto === null ? null : (catalog.models.find((m) => m.model_ref === auto)?.label ?? auto);
  const fallback =
    autoLabel !== null && current === ""
      ? `Not set (uses ${autoLabel} automatically)`
      : slot.slot_id === "chat" && devFake
        ? "Not set (uses the dev fake model)"
        : "Not set";
  return (
    <div className="row">
      <div className="row-main">
        <label htmlFor={id} className="row-title">
          {slot.label}
        </label>
        <span id={`${id}-d`} className="dn-help">
          {slot.description}
        </span>
      </div>
      <Select
        id={id}
        value={current}
        groups={groups}
        placeholder={fallback}
        emptyText="No models yet. Add a provider key below."
        aria-describedby={`${id}-d`}
        onValueChange={onChange}
      />
    </div>
  );
}

function ProviderRow({
  api,
  provider,
  onCatalog,
}: {
  api: Api;
  provider: ProviderInfo;
  onCatalog: (c: ModelCatalog) => void;
}) {
  const toast = useToast();
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"save" | "remove" | null>(null);

  const saveKey = async () => {
    if (key.trim() === "") {
      setError("Paste the key first.");
      return;
    }
    setBusy("save");
    setError(null);
    try {
      onCatalog(await api.setProviderKey(provider.id, key));
      setKey("");
      toast({ tone: "success", title: `${provider.name} key saved` });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const removeKey = async () => {
    setBusy("remove");
    try {
      onCatalog(await api.deleteProviderKey(provider.id));
      toast({
        tone: "info",
        title: `${provider.name} key removed`,
        duration: 10_000,
        action: {
          label: "Undo",
          onAction: () => {
            void api
              .restoreProviderKey(provider.id)
              .then((c) => {
                onCatalog(c);
                toast({ tone: "success", title: `${provider.name} key restored` });
              })
              .catch((e: unknown) => toast({ tone: "error", title: errorMessage(e) }));
          },
        },
      });
    } catch (e) {
      toast({ tone: "error", title: `Couldn't remove the key. ${errorMessage(e)}` });
    } finally {
      setBusy(null);
    }
  };

  return (
    <form
      className="row row-form"
      onSubmit={(e) => {
        e.preventDefault();
        void saveKey();
      }}
    >
      <TextField
        className="row-main"
        label={provider.name}
        type="password"
        autoComplete="off"
        spellCheck={false}
        placeholder={provider.has_key ? "Paste a new key to replace it" : "Paste API key"}
        value={key}
        error={error}
        help={`${provider.has_key ? "Key saved." : "No key yet."} Messages in chats that use ${provider.name} models are sent to ${provider.name} under your account with them.`}
        onChange={(e) => {
          setKey(e.target.value);
          if (error !== null) setError(null);
        }}
      />
      <div className="row-actions">
        <Button
          type="submit"
          variant="secondary"
          loading={busy === "save"}
          icon={<KeyRound {...ICON} aria-hidden="true" />}
        >
          Save key
        </Button>
        {provider.has_key && (
          <Button variant="ghost" loading={busy === "remove"} onClick={() => void removeKey()}>
            Remove key
          </Button>
        )}
      </div>
    </form>
  );
}

function GeneralPage({ api, info }: { api: Api; info: AppInfo | null }) {
  const toast = useToast();
  const [confirming, setConfirming] = useState(false);
  const [summary, setSummary] = useState<LocalDataSummary | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const openConfirm = async () => {
    setDeleteError(null);
    setSummary(null);
    setConfirming(true);
    try {
      setSummary(await api.localDataSummary());
    } catch (e) {
      setDeleteError(`Couldn't check what is stored. ${errorMessage(e)}`);
    }
  };

  const deleteAll = async () => {
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.deleteAllLocalData();
      setConfirming(false);
      toast({ tone: "success", title: "All local data deleted" });
      // Start over with an empty app (also drops anything the window kept in memory).
      window.setTimeout(() => window.location.reload(), 600);
    } catch (e) {
      setDeleteError(errorMessage(e));
    } finally {
      setDeleting(false);
    }
  };

  const dataDir = summary?.data_dir ?? info?.data_dir ?? null;
  const modelsDir = summary?.models_dir ?? info?.models_dir ?? null;

  return (
    <>
      <section className="card" aria-labelledby="privacy-h">
        <h2 id="privacy-h">Privacy</h2>
        <p className="dn-help">Demios collects no usage data. There is no Demios account and no Demios server.</p>
        <h3 id="data-flow-h" className="card-subhead">
          Where your data goes
        </h3>
        <ul className="data-flow" aria-labelledby="data-flow-h">
          <li>
            <span className="row-title">On this computer</span>
            <span className="dn-help">
              Chats and settings are stored in {info?.data_dir ?? "the app data folder"}. Provider keys and sign-ins are
              kept in your system keychain. Local models answer without sending anything off this computer.
            </span>
          </li>
          <li>
            <span className="row-title">Model providers you add a key for</span>
            <span className="dn-help">
              Messages in chats that use a provider's models are sent to that provider under your account with them, and
              their terms apply.
            </span>
          </li>
          <li>
            <span className="row-title">Hugging Face</span>
            <span className="dn-help">Downloading a model fetches it from Hugging Face. Your chats are never sent there.</span>
          </li>
          <li>
            <span className="row-title">Tool servers (MCP)</span>
            <span className="dn-help">
              A tool server you add receives what its tools are asked to do, which can include text from your chat.
              Read-only tools run without asking. Tools that change something ask for your approval first, unless you
              allowed that tool for the chat.
            </span>
          </li>
        </ul>
        <div className="row">
          <div className="row-main">
            <span className="row-title">Delete all local data</span>
            <span className="dn-help">
              Chats, settings, tool history, downloaded models, and the keys and sign-ins Demios saved in your keychain.
            </span>
          </div>
          <div className="row-actions">
            <Button
              ref={triggerRef}
              variant="danger"
              icon={<Trash2 {...ICON} aria-hidden="true" />}
              onClick={() => void openConfirm()}
            >
              Delete all local data
            </Button>
          </div>
        </div>
      </section>
      <section className="card" aria-labelledby="about-h">
        <h2 id="about-h">About</h2>
        <div className="row">
          <div className="row-main">
            <span className="row-title">Demios Desktop {info?.version ?? ""}</span>
            <span className="dn-help">Your chats are stored in {info?.data_dir ?? "the app data folder"}</span>
            {info?.models_dir != null && <span className="dn-help">Downloaded models are stored in {info.models_dir}</span>}
          </div>
        </div>
      </section>
      <ConfirmDialog
        open={confirming}
        onOpenChange={(o) => {
          if (!deleting) setConfirming(o);
        }}
        title="Delete all local data?"
        confirmLabel="Delete all data"
        busy={deleting}
        ready={summary !== null}
        error={deleteError}
        onConfirm={() => void deleteAll()}
        returnFocusTo={triggerRef}
      >
        <p>This deletes from this computer:</p>
        <ul aria-busy={summary === null || undefined}>
          <li>
            The database{dataDir !== null ? ` in ${dataDir}` : ""}:{" "}
            {summary === null
              ? "every chat"
              : `${summary.conversations} ${summary.conversations === 1 ? "chat" : "chats"}`}
            {" "}(and recently deleted ones),{" "}
            {summary === null
              ? "their artifacts"
              : `${summary.artifacts} ${summary.artifacts === 1 ? "artifact" : "artifacts"}`}
            , settings and tool history
          </li>
          <li>
            Downloaded models{modelsDir !== null ? ` in ${modelsDir}` : ""}
            {summary !== null && (summary.models_bytes > 0 ? ` (${formatBytes(summary.models_bytes)})` : " (none)")}, also
            the ones the <code>dn</code> command-line tool uses
          </li>
          <li>
            Keys in your system keychain
            {summary !== null && (summary.keychain.length > 0 ? `: ${summary.keychain.join(", ")}` : ": none saved")}
          </li>
          <li>App storage of this window</li>
        </ul>
        <p>
          <strong>This can't be undone.</strong> Your accounts with providers are not affected.
        </p>
      </ConfirmDialog>
    </>
  );
}
