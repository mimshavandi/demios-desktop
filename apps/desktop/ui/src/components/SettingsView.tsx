import { Cpu, KeyRound } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type AppInfo,
  type ModelCatalog,
  type ProviderInfo,
  type Settings,
  type SlotInfo,
} from "../ipc/types";
import { Badge, Button, ICON, InlineMessage, Select, Skeleton, Switch, TextField, type SelectGroup } from "./primitives";
import { useToast } from "./toast";

export type SettingsSection = "models" | "general";

interface Props {
  api: Api;
  info: AppInfo | null;
  section: SettingsSection;
  onSection: (s: SettingsSection) => void;
  /** Keeps the shell's copy of the catalog (model badge, composer state) current. */
  onCatalog: (c: ModelCatalog) => void;
}

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; catalog: ModelCatalog };

export function SettingsView({ api, info, section, onSection, onCatalog }: Props) {
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
            catalog={load.catalog}
            onCatalog={set}
            onSave={(next, what) => void save(load.catalog, next, what)}
          />
        )}
        {load.kind === "ready" && section === "general" && (
          <GeneralPage
            info={info}
            settings={load.catalog.settings}
            onSave={(next, what) => void save(load.catalog, next, what)}
          />
        )}
        </div>
      </div>
    </section>
  );
}

function ModelsPage({
  api,
  info,
  catalog,
  onCatalog,
  onSave,
}: {
  api: Api;
  info: AppInfo | null;
  catalog: ModelCatalog;
  onCatalog: (c: ModelCatalog) => void;
  onSave: (s: Settings, what: string) => void;
}) {
  const s = catalog.settings;
  const hasAnyModel = catalog.models.length > 0;
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

      <section className="card" aria-labelledby="local-h">
        <h2 id="local-h">On this device</h2>
        {!hasAnyModel && catalog.local_models.length === 0 && (
          <p className="dn-help">Download a model or add a key.</p>
        )}
        {info?.dev_fake_model === true && (
          <div className="row">
            <div className="row-main">
              <span className="row-title">Dev fake model</span>
              <span className="dn-help">
                Streams a canned reply so the app works before real models are connected. Development builds only.
              </span>
            </div>
            <Badge tone="local" icon={<Cpu size={12} strokeWidth={1.5} aria-hidden="true" />}>
              Ready
            </Badge>
          </div>
        )}
        {catalog.local_models.map((m) => (
          <div className="row" key={m.model_id}>
            <div className="row-main">
              <span className="row-title">
                {m.label} <span className="dn-help">{m.size}</span>
              </span>
              <span className="dn-help">{m.note}</span>
            </div>
            <Badge>Coming soon</Badge>
          </div>
        ))}
      </section>

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
  const fallback = slot.slot_id === "chat" && devFake ? "Not set (uses the dev fake model)" : "Not set";
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
        help={provider.has_key ? "Key saved" : "No key yet"}
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

function GeneralPage({
  info,
  settings,
  onSave,
}: {
  info: AppInfo | null;
  settings: Settings;
  onSave: (s: Settings, what: string) => void;
}) {
  return (
    <>
      <section className="card" aria-labelledby="privacy-h">
        <h2 id="privacy-h">Privacy</h2>
        <Switch
          label="Share anonymous usage statistics"
          description="Off by default. This build collects nothing either way."
          checked={settings.telemetry_enabled}
          onCheckedChange={(v) => onSave({ ...settings, telemetry_enabled: v }, "Usage statistics setting")}
        />
      </section>
      <section className="card" aria-labelledby="about-h">
        <h2 id="about-h">About</h2>
        <div className="row">
          <div className="row-main">
            <span className="row-title">Demios Desktop {info?.version ?? ""}</span>
            <span className="dn-help">Your chats are stored in {info?.data_dir ?? "the app data folder"}</span>
          </div>
        </div>
      </section>
    </>
  );
}
