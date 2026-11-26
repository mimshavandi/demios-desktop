import { Blocks, Boxes, Cpu, ExternalLink, KeyRound, SlidersHorizontal, Sparkles, Trash2, Wrench } from "lucide-react";
import { Fragment, useCallback, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type AddModelLink,
  type AppInfo,
  type KeychainStatus,
  type LocalDataSummary,
  type McpServerStatus,
  type ModelCatalog,
  type ProviderInfo,
  type Settings,
  type SlotInfo,
  type UpdateCheck,
} from "../ipc/types";
import {
  Badge,
  Button,
  ICON,
  InlineMessage,
  Select,
  Skeleton,
  Tabs,
  TextField,
  tabPanelProps,
  type SelectGroup,
  type TabItem,
} from "./primitives";
import { ConfirmDialog } from "./ConfirmDialog";
import { AddonsPage } from "./Addons";
import { Licences } from "./Licences";
import { ConnectCard } from "./Connect";
import { McpServersCard } from "./McpServers";
import { RemoteServersCard } from "./RemoteServers";
import { SkillsCard } from "./Skills";
import { Updates } from "./Updates";
import { LocalModelsCard, formatBytes } from "./LocalModels";
import { DefaultsContext, SetDefault, type Defaults } from "./SetDefault";
import { useToast } from "./toast";

export type SettingsSection = "general" | "models" | "tools" | "skills" | "addons";
export type ModelsTab = "device" | "servers" | "providers";

/** Where Settings opens: a section, and for Models which tab. Banners and hints link here. */
export interface SettingsRoute {
  section: SettingsSection;
  tab?: ModelsTab;
}

const SECTIONS: { id: SettingsSection; label: string; icon: typeof Cpu }[] = [
  { id: "general", label: "General", icon: SlidersHorizontal },
  { id: "models", label: "Models", icon: Boxes },
  { id: "tools", label: "Tools", icon: Wrench },
  { id: "skills", label: "Skills", icon: Sparkles },
  // Only in a build with add-ons (ADR-0013); the regular build never shows it.
  { id: "addons", label: "Add-ons", icon: Blocks },
];

const MODEL_TABS: TabItem<ModelsTab>[] = [
  { id: "device", label: "On this device" },
  { id: "servers", label: "Model servers" },
  { id: "providers", label: "Providers" },
];

const ROUTE_PREF = "dn.settings";

/** The last Settings page the user had open (a per-window convenience; may be unavailable). */
export function lastSettingsRoute(): SettingsRoute {
  try {
    const v = JSON.parse(localStorage.getItem(ROUTE_PREF) ?? "null") as Partial<SettingsRoute> | null;
    const section = SECTIONS.find((x) => x.id === v?.section)?.id ?? "general";
    const tab = MODEL_TABS.find((t) => t.id === v?.tab)?.id;
    return tab === undefined ? { section } : { section, tab };
  } catch {
    return { section: "general" };
  }
}

function rememberRoute(r: SettingsRoute) {
  try {
    localStorage.setItem(ROUTE_PREF, JSON.stringify(r));
  } catch {
    // A convenience only: Settings still opens, on General.
  }
}

/** The privacy policy and accessibility statement on demios.io (T-STORE-05, T-STORE-13). */
export const PRIVACY_URL = "https://demios.io/privacy";
export const ACCESSIBILITY_URL = "https://demios.io/accessibility";

interface Props {
  api: Api;
  info: AppInfo | null;
  route: SettingsRoute;
  onRoute: (r: SettingsRoute) => void;
  /** Keeps the shell's copy of the catalog (model badge, composer state) current. */
  onCatalog: (c: ModelCatalog) => void;
  /** MCP tool servers and their connection state. */
  servers: McpServerStatus[];
  /** A demios:// link waiting for confirmation (T-INF-12). */
  addLink?: AddModelLink | null;
  onAddLinkClosed?: () => void;
  /** A newer version the launch check found (#95), shown in About › Updates. */
  launchUpdate?: UpdateCheck | null;
  /** Bring About › Updates into view (opened from the launch notice). */
  revealUpdates?: boolean;
}

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; catalog: ModelCatalog };

/**
 * Settings (#174): a section list on the left (a select on narrow windows) with General,
 * Models, Tools and Skills. Models has its own tabs. The open page is remembered, and
 * banners and hints elsewhere open a page directly through `route`.
 */
export function SettingsView({
  api,
  info,
  route,
  onRoute,
  onCatalog,
  servers,
  addLink = null,
  onAddLinkClosed,
  launchUpdate = null,
  revealUpdates = false,
}: Props) {
  const toast = useToast();
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  // Settings › Tools: Connect and Tool servers reload when the other changes the servers.
  const [toolsVersion, setToolsVersion] = useState(0);
  const toolsChanged = useCallback(() => setToolsVersion((v) => v + 1), []);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const navRef = useRef<HTMLUListElement>(null);
  // Add-ons: only when this build has some. A failure to ask hides the entry and says so.
  const [hasAddons, setHasAddons] = useState(false);
  useEffect(() => {
    api
      .addonsList()
      .then((l) => setHasAddons(l.length > 0))
      .catch((e: unknown) => {
        setHasAddons(false);
        toast({ tone: "error", title: `Couldn't check for add-ons. ${errorMessage(e)}` });
      });
  }, [api, toast]);
  const sections = SECTIONS.filter((x) => x.id !== "addons" || hasAddons);
  const section = sections.some((x) => x.id === route.section) ? route.section : "general";
  const tab: ModelsTab = route.tab ?? "device";
  const title = SECTIONS.find((x) => x.id === section)?.label ?? "Settings";

  useEffect(() => rememberRoute(route), [route]);

  // A new page: its heading takes focus (design-system §7), unless the user is moving
  // through the section list with the arrow keys.
  const keyNav = useRef(false);
  useEffect(() => {
    if (keyNav.current) {
      keyNav.current = false;
      return;
    }
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
  useKeychainAnswer(api, load.kind === "ready" ? load.catalog : null, set, (message) =>
    setLoad({ kind: "error", message }),
  );

  const save = async (catalog: ModelCatalog, next: Settings, what: string, done?: string) => {
    try {
      const settings = await api.updateSettings(next);
      set({ ...catalog, settings });
      toast({ tone: "success", title: done ?? `${what} saved` });
    } catch (e) {
      toast({ tone: "error", title: `Couldn't save ${what.toLowerCase()}. ${errorMessage(e)}` });
    }
  };

  const go = (id: SettingsSection) => onRoute(id === "models" ? { section: id, tab } : { section: id });

  // Up/Down (and Home/End) move between sections and open them; Tab leaves the list.
  const onNavKey = (e: KeyboardEvent<HTMLUListElement>) => {
    const i = sections.findIndex((x) => x.id === section);
    const to =
      e.key === "ArrowDown" ? Math.min(i + 1, sections.length - 1)
      : e.key === "ArrowUp" ? Math.max(i - 1, 0)
      : e.key === "Home" ? 0
      : e.key === "End" ? sections.length - 1
      : null;
    if (to === null) return;
    e.preventDefault();
    const next = sections[to];
    if (next === undefined || next.id === section) return;
    keyNav.current = true;
    go(next.id);
    requestAnimationFrame(() =>
      navRef.current?.querySelector<HTMLButtonElement>(`[data-section="${next.id}"]`)?.focus(),
    );
  };

  const catalogState = (
    <>
      {load.kind === "loading" && (
        <div className="card" aria-busy="true" aria-label="Loading models">
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
          Couldn't load your models. {load.message}
        </InlineMessage>
      )}
    </>
  );

  return (
    <section className="settings" aria-labelledby="settings-h">
      <div className="settings-layout">
        <nav className="settings-nav" aria-label="Settings sections">
          <ul ref={navRef} onKeyDown={onNavKey}>
            {sections.map(({ id, label, icon: Icon }) => (
              <li key={id}>
                <button
                  type="button"
                  className={id === section ? "nav active" : "nav"}
                  aria-current={id === section ? "page" : undefined}
                  data-section={id}
                  data-testid={`settings-nav-${id}`}
                  tabIndex={id === section ? 0 : -1}
                  onClick={() => go(id)}
                >
                  <Icon {...ICON} aria-hidden="true" />
                  {label}
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <div className="settings-column">
          {/* Narrow windows: the section list becomes this select (CSS shows one of the two). */}
          <div className="settings-select">
            <label htmlFor="settings-section" className="dn-visually-hidden">
              Settings section
            </label>
            <select
              id="settings-section"
              className="dn-select dn-native-select"
              value={section}
              onChange={(e) => go(e.target.value as SettingsSection)}
            >
              {sections.map((x) => (
                <option key={x.id} value={x.id}>
                  {x.label}
                </option>
              ))}
            </select>
          </div>
          <h1 id="settings-h" ref={headingRef} tabIndex={-1}>
            {title}
          </h1>

          {section === "general" && (
            <GeneralPage api={api} info={info} launchUpdate={launchUpdate} revealUpdates={revealUpdates} />
          )}

          {section === "models" && (
            <>
              {catalogState}
              {load.kind === "ready" && (
                <ModelsPage
                  api={api}
                  info={info}
                  catalog={load.catalog}
                  tab={tab}
                  onTab={(t) => onRoute({ section: "models", tab: t })}
                  onCatalog={set}
                  onSave={(next, what, done) => void save(load.catalog, next, what, done)}
                  addLink={addLink}
                  {...(onAddLinkClosed !== undefined ? { onAddLinkClosed } : {})}
                />
              )}
            </>
          )}

          {section === "tools" && (
            <>
              <ConnectCard api={api} live={servers} version={toolsVersion} onChanged={toolsChanged} />
              <McpServersCard api={api} live={servers} version={toolsVersion} onChanged={toolsChanged} />
            </>
          )}

          {section === "skills" && <SkillsCard api={api} />}

          {section === "addons" && <AddonsPage api={api} />}
        </div>
      </div>
    </section>
  );
}

function ModelsPage({
  api,
  info,
  catalog,
  tab,
  onTab,
  onCatalog,
  onSave,
  addLink,
  onAddLinkClosed,
}: {
  addLink: AddModelLink | null;
  onAddLinkClosed?: () => void;
  api: Api;
  info: AppInfo | null;
  catalog: ModelCatalog;
  tab: ModelsTab;
  onTab: (t: ModelsTab) => void;
  onCatalog: (c: ModelCatalog) => void;
  onSave: (s: Settings, what: string, done?: string) => void;
}) {
  const s = catalog.settings;
  // A download finished: pickers and the composer should see the new model.
  const toast = useToast();
  const refreshCatalog = useCallback(() => {
    void api
      .listModels()
      .then(onCatalog)
      // Say so: the pickers would otherwise miss the model that just finished (#141).
      .catch((e: unknown) =>
        toast({ tone: "error", title: `Couldn't refresh the model list. ${errorMessage(e)}` }),
      );
  }, [api, onCatalog, toast]);
  // A demios:// add-model link opens On this device, where the add panel is.
  const shown: ModelsTab = addLink !== null ? "device" : tab;
  const defaults: Defaults = {
    catalog,
    setDefault: (slot, modelRef) =>
      onSave({ ...s, slots: { ...s.slots, [slot.slot_id]: modelRef } }, `${slot.label} model`, `Now used for ${slot.label}`),
  };
  return (
    <DefaultsContext.Provider value={defaults}>
      <section className="card" aria-labelledby="slots-h">
        <h2 id="slots-h">Model for each task</h2>
        <p className="dn-help">A task without a model shows a message instead of quietly using another one.</p>
        {catalog.slots.map((slot) => (
          <SlotRow
            key={slot.slot_id}
            slot={slot}
            catalog={catalog}
            {...(slot.slot_id === "embed" && (s.slots["embed"] ?? "") === ""
              ? { note: <p className="dn-help row-note">No model chosen: project files are searched by keywords.</p> }
              : {})}
            onChange={(model) => {
              const slots = { ...s.slots };
              if (model === "") delete slots[slot.slot_id];
              else slots[slot.slot_id] = model;
              onSave({ ...s, slots }, `${slot.label} model`);
            }}
          />
        ))}
      </section>

      <Tabs label="Where models come from" idBase="models" items={MODEL_TABS} value={shown} onChange={onTab} />
      <div {...tabPanelProps("models", shown)}>
        {shown === "device" && (
          <>
            <LocalModelsCard
              api={api}
              initial={catalog.local_models}
              compute={catalog.compute}
              onReady={refreshCatalog}
              link={addLink}
              {...(onAddLinkClosed !== undefined ? { onLinkClosed: onAddLinkClosed } : {})}
            />
            {info?.models_dir != null && (
              <p className="dn-help settings-note">Downloaded models are stored in {info.models_dir}</p>
            )}
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
          </>
        )}
        {shown !== "device" && <KeychainNotice api={api} status={catalog.keychain} onCatalog={onCatalog} />}
        {shown === "servers" && <RemoteServersCard api={api} catalog={catalog} onCatalog={onCatalog} />}
        {shown === "providers" && (
          <section className="card" aria-labelledby="keys-h">
            <h2 id="keys-h">Provider keys</h2>
            <p className="dn-help">
              Bring your own key. Keys are never shown again after you save them. Storage: {catalog.key_store}. Messages
              in chats that use a provider's models are sent to that provider under your account with them.
            </p>
            {catalog.providers.map((p) => (
              <Fragment key={p.id}>
                <ProviderRow api={api} provider={p} onCatalog={onCatalog} />
                {p.key === "saved" && <ProviderModels catalog={catalog} provider={p} />}
              </Fragment>
            ))}
          </section>
        )}
      </div>
    </DefaultsContext.Provider>
  );
}

/**
 * The catalog came back before the keychain answered (#200): wait for its answer, however long
 * the user takes with an unlock prompt, and hand it on. The backend runs one check at a time, so
 * every caller gets the same answer from one prompt.
 */
export function useKeychainAnswer(
  api: Api,
  catalog: ModelCatalog | null,
  onAnswer: (c: ModelCatalog) => void,
  onError: (message: string) => void,
) {
  const checking = catalog?.keychain.state === "checking";
  const handlers = useRef({ onAnswer, onError });
  handlers.current = { onAnswer, onError };
  useEffect(() => {
    if (!checking) return;
    let live = true;
    api
      .checkKeys()
      .then((c) => {
        if (live) handlers.current.onAnswer(c);
      })
      .catch((e: unknown) => {
        if (live) handlers.current.onError(errorMessage(e));
      });
    return () => {
      live = false;
    };
  }, [api, checking]);
}

/** After this long without an answer, "checking" becomes "locked or not answering". */
export const KEYCHAIN_SLOW_MS = 8000;

/**
 * The system keychain didn't answer or failed (#200): say why and what to do, instead of showing
 * "no key". While it is still being checked (an unlock prompt may be open), say so; after
 * {@link KEYCHAIN_SLOW_MS} say it is locked or not answering. The check itself is never cut short.
 */
export function KeychainNotice({
  api,
  status,
  onCatalog,
}: {
  api: Api;
  status: KeychainStatus;
  onCatalog: (c: ModelCatalog) => void;
}) {
  const [slow, setSlow] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setSlow(false);
    if (status.state !== "checking") return;
    const t = setTimeout(() => setSlow(true), KEYCHAIN_SLOW_MS);
    return () => clearTimeout(t);
  }, [status.state]);
  if (status.state === "ok") return null;
  if (status.state === "checking" && !slow) {
    return <InlineMessage tone="info">Checking your system keychain for saved keys…</InlineMessage>;
  }
  const retry = async () => {
    setBusy(true);
    setError(null);
    try {
      onCatalog(await api.checkKeys());
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const message =
    status.state === "checking"
      ? "Your system keychain is locked or not answering, so Demios can't tell which API keys are saved. If it asked for a password, answer it, then choose Retry."
      : status.message;
  return (
    <InlineMessage
      tone="warning"
      action={
        <Button size="sm" variant="secondary" loading={busy} onClick={() => void retry()}>
          Retry
        </Button>
      }
    >
      {message}
      {error !== null && ` ${error}`}
    </InlineMessage>
  );
}

function SlotRow({
  slot,
  catalog,
  note,
  onChange,
}: {
  slot: SlotInfo;
  catalog: ModelCatalog;
  /** Shown under the selector (the Project search model's offer and download). */
  note?: ReactNode;
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
    ...catalog.remote_servers.map((r) => ({
      label: r.name,
      options: usable
        .filter((m) => m.model_ref.startsWith(`remote:${r.id}:`))
        .map((m) => ({ value: m.model_ref, label: m.label.replace(`${r.name} · `, "") })),
    })),
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
        placeholder="Not set"
        emptyText="No models yet. Download one under On this device, or add a key under Providers."
        aria-describedby={`${id}-d`}
        onValueChange={onChange}
      />
      {note}
    </div>
  );
}

/** A provider's models, once its key is saved, each with "Set as default" (#219). */
function ProviderModels({ catalog, provider }: { catalog: ModelCatalog; provider: ProviderInfo }) {
  const models = catalog.models.filter((m) => m.model_ref.startsWith(`byok:${provider.id}:`));
  if (models.length === 0) return null;
  return (
    <ul className="provider-models" aria-label={`${provider.name} models`}>
      {models.map((m) => {
        const name = m.label.replace(`${provider.name} · `, "");
        return (
          <li key={m.model_ref} className="row" data-testid="provider-model">
            <div className="row-main">
              <span className="row-title">{name}</span>
              {!m.available && <span className="dn-help">{m.note ?? "Not in this build yet"}</span>}
            </div>
            <span className="row-actions">
              <SetDefault modelRef={m.model_ref} name={name} />
            </span>
          </li>
        );
      })}
    </ul>
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
        placeholder={provider.key === "saved" ? "Paste a new key to replace it" : "Paste API key"}
        value={key}
        error={error}
        help={
          provider.key === "saved"
            ? "Key saved."
            : provider.key === "none"
              ? "No key yet."
              : "Can't tell whether a key is saved: see the keychain message above."
        }
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
        {provider.key === "saved" && (
          <Button variant="ghost" loading={busy === "remove"} onClick={() => void removeKey()}>
            Remove key
          </Button>
        )}
      </div>
    </form>
  );
}

function GeneralPage({
  api,
  info,
  launchUpdate,
  revealUpdates,
}: {
  api: Api;
  info: AppInfo | null;
  launchUpdate: UpdateCheck | null;
  revealUpdates: boolean;
}) {
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

  const reportProblem = async () => {
    try {
      await api.reportProblem("problem");
      toast({ tone: "info", title: "Opened the report form in your browser" });
    } catch (e) {
      toast({ tone: "error", title: errorMessage(e) });
    }
  };

  // Through the same native "Open link?" confirm as links in answers (`open_link`).
  const openPage = async (url: string) => {
    try {
      await api.openLink(url);
    } catch (e) {
      toast({ tone: "error", title: `Couldn't open the page. ${errorMessage(e)}` });
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
      <section className="card" aria-labelledby="about-h">
        <h2 id="about-h">About</h2>
        <div className="row">
          <div className="row-main">
            <span className="row-title">Demios Desktop {info?.version ?? ""}</span>
            {(info?.channel != null || info?.commit != null) && (
              <span className="dn-help">
                {[info.channel === "preview" ? "Preview build (unsigned)" : info.channel, info.commit && `commit ${info.commit}`]
                  .filter(Boolean)
                  .join(", ")}
              </span>
            )}
            <span className="dn-help">
              Answers come from the AI model you choose, on this computer or from a provider you add a key for. Demios
              doesn't check them: they can be wrong or made up, so verify anything important.
            </span>
          </div>
        </div>
        {info !== null && (
          <Updates api={api} mode={info.updates} initial={launchUpdate} reveal={revealUpdates} />
        )}
      </section>

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
          {info?.updates === "self_update" && (
            <li>
              <span className="row-title">GitHub, for updates</span>
              <span className="dn-help">
                When you select Check for updates, once when Demios starts if you turned that on, and the download after
                you confirm an update. Nothing else is sent.
              </span>
            </li>
          )}
        </ul>
      </section>

      <section className="card" aria-labelledby="help-h">
        <h2 id="help-h">Help and legal</h2>
        <ul className="link-list">
          <li>
            <Button icon={<ExternalLink {...ICON} aria-hidden="true" />} onClick={() => void reportProblem()}>
              Report a problem
            </Button>
            <span className="dn-help">
              A form on GitHub with the app version and your operating system filled in. Nothing from your chats is sent.
              To report an answer, use the flag under it.
            </span>
          </li>
          <li>
            <Button icon={<ExternalLink {...ICON} aria-hidden="true" />} onClick={() => void openPage(PRIVACY_URL)}>
              Privacy policy
            </Button>
            <span className="dn-help">What Demios does with your data, on demios.io.</span>
          </li>
          <li>
            <Button icon={<ExternalLink {...ICON} aria-hidden="true" />} onClick={() => void openPage(ACCESSIBILITY_URL)}>
              Accessibility statement
            </Button>
            <span className="dn-help">How accessible Demios is and how to report a barrier, on demios.io.</span>
          </li>
        </ul>
        <p className="dn-help">Demios asks before it opens a page in your browser.</p>
        <Licences />
      </section>

      <section className="card card-danger" aria-labelledby="delete-h">
        <h2 id="delete-h">Delete local data</h2>
        <div className="row">
          <div className="row-main">
            <span className="dn-help">
              Chats, projects, settings, tool history, downloaded models, and the keys and sign-ins Demios saved in your
              keychain. You can't undo this.
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
            {summary === null
              ? "Projects"
              : `${summary.projects} ${summary.projects === 1 ? "project" : "projects"}`}
            , with their instructions and the copies of their files
          </li>
          <li>
            Downloaded models{modelsDir !== null ? ` in ${modelsDir}` : ""}
            {summary !== null && (summary.models_bytes > 0 ? ` (${formatBytes(summary.models_bytes)})` : " (none)")}, also
            the ones the <code>dn</code> command-line tool uses
          </li>
          <li>
            Keys in your system keychain
            {summary !== null &&
              (summary.keychain.entries.length > 0
                ? `: ${summary.keychain.entries.join(", ")}`
                : summary.keychain.problem === null
                  ? ": none saved"
                  : "")}
            {summary?.keychain.problem != null && (
              <InlineMessage tone="warning">
                Couldn't check all of them. {summary.keychain.problem}
              </InlineMessage>
            )}
          </li>
          <li>App storage of this window</li>
        </ul>
        <p>
          <strong>This can't be undone.</strong> Your accounts with providers are not affected. Skills you wrote
          yourself stay in your skills folder.
        </p>
      </ConfirmDialog>
    </>
  );
}
