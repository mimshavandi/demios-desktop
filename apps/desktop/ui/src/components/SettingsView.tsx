import { useCallback, useEffect, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type AppInfo,
  type ModelCatalog,
  type ProviderInfo,
  type Settings,
  type SlotInfo,
} from "../ipc/types";

interface Props {
  api: Api;
  info: AppInfo | null;
  onClose: () => void;
}

export function SettingsView({ api, info, onClose }: Props) {
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setCatalog(await api.listModels());
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (next: Settings, what: string) => {
    setError(null);
    try {
      const settings = await api.updateSettings(next);
      setCatalog((c) => (c === null ? c : { ...c, settings }));
      setSaved(what);
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  if (catalog === null) {
    return (
      <section className="settings" aria-label="Settings">
        <p>{error ?? "Loading settings…"}</p>
      </section>
    );
  }
  const s = catalog.settings;

  return (
    <section className="settings" aria-label="Settings">
      <header className="settings-head">
        <h1>Settings</h1>
        <button type="button" className="btn btn-ghost" onClick={onClose}>
          Back to chat
        </button>
      </header>
      {error !== null && (
        <div className="banner banner-error" role="alert">
          {error}
        </div>
      )}
      {saved !== null && error === null && (
        <div className="banner banner-ok" role="status">
          {saved} saved.
        </div>
      )}

      <h2>Models</h2>
      <section className="card" aria-labelledby="slots-h">
        <h3 id="slots-h">Model for each task</h3>
        <p className="hint">
          A task without a model shows an error instead of silently using another model.
        </p>
        {catalog.slots.map((slot) => (
          <SlotRow
            key={slot.slot_id}
            slot={slot}
            catalog={catalog}
            onChange={(model) => {
              const slots = { ...s.slots };
              if (model === "") delete slots[slot.slot_id];
              else slots[slot.slot_id] = model;
              void save({ ...s, slots }, `${slot.label} model`);
            }}
          />
        ))}
      </section>

      <section className="card" aria-labelledby="local-h">
        <h3 id="local-h">Local models</h3>
        {info?.dev_fake_model === true && (
          <div className="model-row">
            <div>
              <strong>Dev fake model</strong>
              <div className="hint">
                Canned, streamed replies for development. Used for chat when no model is
                chosen. Built with the <code>dev-fake-model</code> feature.
              </div>
            </div>
            <span className="badge badge-ok">Ready</span>
          </div>
        )}
        {catalog.local_models.map((m) => (
          <div className="model-row" key={m.model_id}>
            <div>
              <strong>{m.label}</strong> <span className="hint">{m.size}</span>
              <div className="hint">{m.note}</div>
            </div>
            <button type="button" className="btn btn-ghost" disabled title={m.note}>
              Download
            </button>
          </div>
        ))}
      </section>

      <section className="card" aria-labelledby="keys-h">
        <h3 id="keys-h">Provider keys (bring your own key)</h3>
        <p className="hint">Stored: {catalog.key_store}. Keys are never shown again after saving.</p>
        {catalog.providers.map((p) => (
          <ProviderRow
            key={p.id}
            provider={p}
            onSave={async (key) => {
              setError(null);
              try {
                setCatalog(await api.setProviderKey(p.id, key));
                setSaved(`${p.name} key`);
                return true;
              } catch (e) {
                setError(errorMessage(e));
                return false;
              }
            }}
            onRemove={async () => {
              setError(null);
              try {
                setCatalog(await api.deleteProviderKey(p.id));
                setSaved(`Removal of the ${p.name} key`);
              } catch (e) {
                setError(errorMessage(e));
              }
            }}
          />
        ))}
      </section>

      <h2>Privacy</h2>
      <section className="card">
        <label className="toggle">
          <input
            type="checkbox"
            checked={s.telemetry_enabled}
            onChange={(e) =>
              void save({ ...s, telemetry_enabled: e.target.checked }, "Telemetry setting")
            }
          />
          <span>
            Share anonymous usage statistics
            <span className="hint block">
              Off by default. This build collects nothing either way.
            </span>
          </span>
        </label>
      </section>

      <p className="hint about">
        Demios Desktop {info?.version ?? ""} · data in {info?.data_dir ?? "the app-data folder"}
      </p>
    </section>
  );
}

function SlotRow({
  slot,
  catalog,
  onChange,
}: {
  slot: SlotInfo;
  catalog: ModelCatalog;
  onChange: (model: string) => void;
}) {
  const current = catalog.settings.slots[slot.slot_id] ?? "";
  const options = catalog.models.filter((m) => m.capabilities.includes(slot.capability));
  const id = `slot-${slot.slot_id}`;
  const known = current === "" || options.some((o) => o.model_ref === current);
  return (
    <div className="model-row">
      <div>
        <label htmlFor={id}>
          <strong>{slot.label}</strong>
        </label>
        <div className="hint">{slot.description}</div>
      </div>
      <select id={id} value={current} onChange={(e) => onChange(e.target.value)}>
        <option value="">Not set</option>
        {!known && <option value={current}>{current} (unavailable)</option>}
        {options.map((o) => (
          <option key={o.model_ref} value={o.model_ref} disabled={!o.available}>
            {o.label}
            {o.available ? "" : " — not in this build yet"}
          </option>
        ))}
      </select>
    </div>
  );
}

function ProviderRow({
  provider,
  onSave,
  onRemove,
}: {
  provider: ProviderInfo;
  onSave: (key: string) => Promise<boolean>;
  onRemove: () => Promise<void>;
}) {
  const [key, setKey] = useState("");
  const id = `key-${provider.id}`;
  return (
    <form
      className="model-row"
      onSubmit={(e) => {
        e.preventDefault();
        void onSave(key).then((ok) => ok && setKey(""));
      }}
    >
      <div>
        <label htmlFor={id}>
          <strong>{provider.name}</strong>
        </label>
        <div className="hint">{provider.has_key ? "Key saved." : "No key."}</div>
      </div>
      <div className="key-input">
        <input
          id={id}
          type="password"
          autoComplete="off"
          spellCheck={false}
          placeholder={provider.has_key ? "Replace key" : "Paste API key"}
          value={key}
          onChange={(e) => setKey(e.target.value)}
        />
        <button type="submit" className="btn btn-primary" disabled={key.trim() === ""}>
          Save
        </button>
        {provider.has_key && (
          <button type="button" className="btn btn-ghost" onClick={() => void onRemove()}>
            Remove
          </button>
        )}
      </div>
    </form>
  );
}
