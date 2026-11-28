// Add a model from Hugging Face (T-INF-10, issue #48): search GGUF repos, pick a
// quantisation with its fit, see the licence and the model card first, then download
// (verified against the published checksum, resumable). States per design-system §4–§5:
// loading, empty, error with Retry, "Checking model…" while the repo's details are read.

import { ArrowLeft, Copy, Cpu, Download, KeyRound, Search } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import { downloadKey, type HubDownload } from "../hubDownloads";
import {
  errorMessage,
  type AddModelLink,
  type HubLicense,
  type HubRepoSummary,
  type HubRepoView,
  type HubSort,
  type HubVariant,
  type ModelTraits,
} from "../ipc/types";
import { FIT, RecommendedBadge, SMALL, formatBytes } from "./LocalModels";
import { Badge, Button, ICON, IconButton, InlineMessage, Skeleton, TextField } from "./primitives";
import { useToast } from "./toast";

type Results =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; query: string; items: HubRepoSummary[]; next: string | null; more: boolean };

const SORTS: { value: HubSort; label: string }[] = [
  { value: "downloads", label: "Most downloaded" },
  { value: "likes", label: "Most liked" },
  { value: "trending", label: "Trending" },
  { value: "updated", label: "Recently updated" },
];

/** The search text, kept while the app runs (the dialog closes and opens again). */
let lastQuery = "";

/** Tests only: forget the kept search text. */
export function forgetHubSearch(): void {
  lastQuery = "";
}

/**
 * The add-model dialog's content (#174): search Hugging Face, open a repo, pick a version.
 * Download hands the version to `onDownload` (the On this device list runs it and shows its
 * progress) and the dialog closes. Opened by a demios:// link, it starts on that model.
 */
export function AddFromHub({
  api,
  onClose,
  onDownload,
  isDownloading,
  link = null,
}: {
  api: Api;
  onClose: () => void;
  onDownload: (d: HubDownload) => void;
  /** Whether a version is downloading now (started earlier from this list). */
  isDownloading: (key: string) => boolean;
  /** Opened by a demios:// link: go straight to its confirmation. */
  link?: AddModelLink | null;
}) {
  const [text, setTextState] = useState(lastQuery);
  const setText = (t: string) => {
    lastQuery = t;
    setTextState(t);
  };
  const [sort, setSort] = useState<HubSort>("downloads");
  const [results, setResults] = useState<Results>({ kind: "idle" });
  const [repo, setRepo] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const lastRepoButton = useRef<string | null>(null);

  useEffect(() => {
    // The dialog's title says what this is: start in the search field.
    searchRef.current?.focus();
  }, []);

  const search = async (cursor: string | null = null) => {
    const q = text.trim();
    if (cursor === null) setResults({ kind: "loading" });
    else setResults((r) => (r.kind === "ready" ? { ...r, more: true } : r));
    try {
      const page = await api.hubSearch(q, sort, cursor);
      setResults((r) => ({
        kind: "ready",
        query: q,
        items: cursor !== null && r.kind === "ready" ? [...r.items, ...page.items] : page.items,
        next: page.next,
        more: false,
      }));
    } catch (e) {
      setResults({ kind: "error", message: errorMessage(e) });
    }
  };

  if (link !== null) {
    // From a link: the confirmation only; leaving it closes the panel.
    return (
      <RepoDetail
        api={api}
        repo={link.repo}
        link={link}
        onBack={onClose}
        onDownload={onDownload}
        isDownloading={isDownloading}
      />
    );
  }

  if (repo !== null) {
    return (
      <RepoDetail
        api={api}
        repo={repo}
        onDownload={onDownload}
        isDownloading={isDownloading}
        onBack={() => {
          setRepo(null);
          requestAnimationFrame(() =>
            [...document.querySelectorAll<HTMLButtonElement>("button[data-repo]")]
              .find((b) => b.dataset["repo"] === lastRepoButton.current)
              ?.focus(),
          );
        }}
      />
    );
  }

  return (
    <section className="hub" aria-label="Search Hugging Face">
      <form
        className="hub-search"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          void search();
        }}
      >
        <TextField
          ref={searchRef}
          label="Search models"
          type="search"
          value={text}
          placeholder="e.g. qwen 1.5b instruct"
          help="Only repos with GGUF files that run on this computer are listed."
          onChange={(e) => setText(e.target.value)}
        />
        <div className="dn-field">
          <label htmlFor="hub-sort">Sort</label>
          <select id="hub-sort" className="dn-select" value={sort} onChange={(e) => setSort(e.target.value as HubSort)}>
            {SORTS.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>
        </div>
        <Button type="submit" variant="primary" icon={<Search {...ICON} aria-hidden="true" />} loading={results.kind === "loading"}>
          Search
        </Button>
      </form>

      {results.kind === "loading" && (
        <div aria-busy="true" aria-label="Searching">
          <Skeleton kind="block" />
          <Skeleton kind="block" />
        </div>
      )}
      {results.kind === "error" && (
        <InlineMessage
          tone="error"
          action={
            <Button size="sm" onClick={() => void search()}>
              Retry
            </Button>
          }
        >
          {results.message}
        </InlineMessage>
      )}
      {results.kind === "ready" && (
        <>
          <p className="dn-help" role="status" aria-live="polite">
            {results.items.length === 0
              ? `No GGUF models match “${results.query}”.`
              : `${results.items.length}${results.next !== null ? "+" : ""} ${results.items.length === 1 ? "model" : "models"}`}
          </p>
          {results.items.length > 0 && (
            <ul className="hub-list" aria-label="Search results">
              {results.items.map((r) => (
                <li key={r.id}>
                  <button
                    type="button"
                    className="hub-item"
                    data-repo={r.id}
                    onClick={() => {
                      lastRepoButton.current = r.id;
                      setRepo(r.id);
                    }}
                  >
                    <span className="hub-name">
                      {r.id} <TypeBadge traits={r.traits_hint} />
                      {r.recommended === true && <RecommendedBadge />}
                    </span>
                    <span className="hub-meta">
                      {r.downloads.toLocaleString()} downloads · {r.likes.toLocaleString()} likes
                      {r.license !== null ? ` · ${r.license}` : ""}
                      {r.gated ? " · needs access" : ""}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          {results.next !== null && (
            <Button size="sm" loading={results.more} onClick={() => void search(results.next)}>
              Show more
            </Button>
          )}
        </>
      )}
      <p className="dn-help">
        Searching and downloading contact Hugging Face. Your chats are never sent there.
      </p>
    </section>
  );
}

const LICENSE_NOTE: Record<NonNullable<HubLicense["warning"]>, { tone: "warning" | "info"; text: string }> = {
  missing: { tone: "warning", text: "This model doesn't state a licence. Check the model card before using it." },
  non_commercial: { tone: "warning", text: "This licence doesn't allow commercial use. Check it before downloading." },
  custom: { tone: "warning", text: "This model has its own licence. Read it on the model card before downloading." },
  use_policy: { tone: "info", text: "Commercial use is allowed under an acceptable-use policy. Read it on the model card." },
};

function RepoDetail({
  api,
  repo,
  onBack,
  onDownload,
  isDownloading,
  link = null,
}: {
  api: Api;
  repo: string;
  onBack: () => void;
  onDownload: (d: HubDownload) => void;
  isDownloading: (key: string) => boolean;
  link?: AddModelLink | null;
}) {
  const toast = useToast();
  const [view, setView] = useState<HubRepoView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);

  const load = async () => {
    setError(null);
    setView(null);
    try {
      setView(link !== null ? await api.hubResolveLink(link) : await api.hubRepo(repo));
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  useEffect(() => {
    void load();
    headingRef.current?.focus();
    // Load once per repo.
  }, [repo]);

  const download = (v: HubVariant) => {
    if (view === null) return;
    onDownload({
      repo: view.id,
      sha: view.sha,
      variant: v.name,
      label: `${view.id.split("/")[1] ?? view.id} ${v.quant ?? v.name}`,
      total: v.total_size,
    });
  };

  return (
    <section className="hub" aria-labelledby="hub-repo-h" aria-busy={view === null && error === null}>
      <Button size="sm" variant="ghost" icon={<ArrowLeft {...ICON} aria-hidden="true" />} onClick={onBack}>
        {link !== null ? "Not now" : "Back to results"}
      </Button>
      {link !== null && (
        <InlineMessage tone="info">
          A link asked to add this model. Check it below; nothing downloads until you choose Download.
        </InlineMessage>
      )}
      <h3 id="hub-repo-h" ref={headingRef} tabIndex={-1} className="card-subhead hub-repo-name">
        {repo} <TypeBadge traits={view?.traits} />
      </h3>
      {view === null && error === null && (
        <div>
          <p className="dn-help" role="status">
            Checking model… Reading its files and size from Hugging Face. Nothing is downloaded yet.
          </p>
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
          {error}
        </InlineMessage>
      )}
      {view !== null && (
        <>
          <span className="dn-help local-license">
            Licence: {view.license.name ?? view.license.id ?? "not stated"} · Model card:{" "}
            <code>{view.card_url.replace(/^https:\/\//, "")}</code>
            <IconButton
              size="sm"
              label="Copy model card link"
              onClick={() =>
                void navigator.clipboard?.writeText(view.card_url).then(() => toast({ tone: "success", title: "Link copied" }))
              }
            >
              <Copy {...ICON} aria-hidden="true" />
            </IconButton>
          </span>
          {view.fit_unknown && (
            <InlineMessage tone="warning">
              Almend couldn't read this model's details, so it can't tell whether it fits this computer.
            </InlineMessage>
          )}
          {view.license.warning !== null && (
            <InlineMessage tone={LICENSE_NOTE[view.license.warning].tone}>{LICENSE_NOTE[view.license.warning].text}</InlineMessage>
          )}
          {view.gated && <HfToken api={api} onSaved={() => void load()} />}
          {view.variants.length === 0 ? (
            <p className="dn-help">This repo has no GGUF files Almend can run.</p>
          ) : (
            <ul className="hub-variants" aria-label="Versions">
              {[...view.variants]
                // The variant the link chose comes first.
                .sort((a, b) => Number(b.name === view.selected) - Number(a.name === view.selected))
                .map((v) => {
                const fit = v.fit !== null ? FIT[v.fit.status] : null;
                const nameId = `hubv-${v.name}`;
                return (
                  <li key={v.name} className="hub-variant" data-testid="hub-variant" data-selected={v.name === view.selected || undefined}>
                    <div className="row-main">
                      <span className="row-title" id={nameId}>
                        {v.quant ?? v.name}{" "}
                        {v.name === view.selected && <Badge tone="accent">Chosen by the link</Badge>}{" "}
                        {v.recommended && <RecommendedBadge />}{" "}
                        <span className="dn-help">
                          {formatBytes(v.total_size)}
                          {v.parts > 1 ? ` · ${v.parts} files` : ""}
                        </span>
                      </span>
                      <span className="local-meta">
                        {fit !== null && v.fit !== null ? (
                          <>
                            <Badge tone={fit.tone} icon={<Cpu {...SMALL} aria-hidden="true" />}>
                              {fit.label}
                              {v.fit.estimated ? " (estimate)" : ""}
                            </Badge>
                            <span className="dn-help">{v.fit.reason}</span>
                          </>
                        ) : (
                          <span className="dn-help">Fit unknown: its details couldn't be read.</span>
                        )}
                      </span>
                      {!v.verifiable && (
                        <span className="dn-help">No published checksum, so Almend won't download it.</span>
                      )}
                    </div>
                    <div className="row-actions local-actions">
                      {v.downloaded !== null && v.downloaded !== undefined ? (
                        <Badge tone="success">Downloaded</Badge>
                      ) : v.progress !== null || isDownloading(downloadKey({ repo: view.id, sha: view.sha, variant: v.name })) ? (
                        <Badge tone="accent">Downloading, see On this device</Badge>
                      ) : (
                        <Button
                          size="sm"
                          variant="secondary"
                          disabled={!v.verifiable}
                          icon={<Download {...ICON} aria-hidden="true" />}
                          onClick={() => download(v)}
                          aria-describedby={nameId}
                        >
                          Download
                        </Button>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
          <p className="dn-help">
            Download closes this window and shows progress under On this device. Downloads go to your models folder, are checked against Hugging Face's published checksum, and resume if
            interrupted.
          </p>
        </>
      )}
    </section>
  );
}

/** The Hugging Face token, for models that need access. Kept in the system keychain. */
function HfToken({ api, onSaved }: { api: Api; onSaved: () => void }) {
  const toast = useToast();
  const [has, setHas] = useState<boolean | null>(null);
  // Checking the keychain failed: say so instead of claiming there's no token (#141).
  const [statusError, setStatusError] = useState<string | null>(null);
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void api
      .hfTokenStatus()
      .then(setHas)
      .catch((e: unknown) => setStatusError(errorMessage(e)));
  }, [api]);

  return (
    <div className="hub-token">
      <InlineMessage tone="info">
        This model needs access: accept its terms on the model card, then add a Hugging Face token (read access is
        enough). The token is kept in your system keychain and is sent only to Hugging Face.
      </InlineMessage>
      {statusError !== null && has === null && (
        <InlineMessage tone="error">Couldn't check for a saved Hugging Face token. {statusError}</InlineMessage>
      )}
      {has === true ? (
        <div className="row">
          <span className="dn-help">Hugging Face token saved.</span>
          <Button
            size="sm"
            variant="ghost"
            loading={busy}
            onClick={async () => {
              setBusy(true);
              try {
                setHas(await api.deleteHfToken());
                toast({ tone: "info", title: "Hugging Face token removed" });
              } catch (e) {
                toast({ tone: "error", title: errorMessage(e) });
              } finally {
                setBusy(false);
              }
            }}
          >
            Remove token
          </Button>
        </div>
      ) : (
        <form
          className="row row-form"
          onSubmit={async (e) => {
            e.preventDefault();
            setError(null);
            setBusy(true);
            try {
              setHas(await api.setHfToken(value));
              setValue("");
              toast({ tone: "success", title: "Hugging Face token saved" });
              // Access may differ with a token: read the repo again.
              onSaved();
            } catch (err) {
              setError(errorMessage(err));
            } finally {
              setBusy(false);
            }
          }}
        >
          <TextField
            className="row-main"
            label="Hugging Face token"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={value}
            error={error}
            placeholder="hf_…"
            onChange={(e) => setValue(e.target.value)}
          />
          <div className="row-actions">
            <Button type="submit" variant="secondary" loading={busy} icon={<KeyRound {...ICON} aria-hidden="true" />}>
              Save token
            </Button>
          </div>
        </form>
      )}
    </div>
  );
}

/** The model's type (#175): "Chat · Tools", "Embedding"… A hint from the Hub says so. */
function TypeBadge({ traits }: { traits: ModelTraits | null | undefined }) {
  if (traits === null || traits === undefined) return null;
  return (
    <Badge tone={traits.kind === "base" ? "warning" : "neutral"}>
      {traits.label}
      {traits.source === "hint" ? " (from the model page)" : ""}
    </Badge>
  );
}
