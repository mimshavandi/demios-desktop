// Add a model from Hugging Face (T-INF-10, issue #48): search GGUF repos, pick a
// quantisation with its fit, see the licence and the model card first, then download
// (verified against the published checksum, resumable). States per design-system §4–§5:
// loading, empty, error with Retry, "Checking model…" while the repo's details are read.

import { ArrowLeft, Copy, Cpu, Download, KeyRound, Pause, Search } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import {
  errorMessage,
  type HubLicense,
  type HubRepoSummary,
  type HubRepoView,
  type HubSort,
  type HubVariant,
} from "../ipc/types";
import { FIT, SMALL, formatBytes } from "./LocalModels";
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

export function AddFromHub({ api, onClose }: { api: Api; onClose: () => void }) {
  const [text, setText] = useState("");
  const [sort, setSort] = useState<HubSort>("downloads");
  const [results, setResults] = useState<Results>({ kind: "idle" });
  const [repo, setRepo] = useState<string | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const lastRepoButton = useRef<string | null>(null);

  useEffect(() => {
    headingRef.current?.focus();
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

  if (repo !== null) {
    return (
      <RepoDetail
        api={api}
        repo={repo}
        onBack={() => {
          setRepo(null);
          requestAnimationFrame(() =>
            document.querySelector<HTMLButtonElement>(`[data-repo="${lastRepoButton.current ?? ""}"]`)?.focus(),
          );
        }}
      />
    );
  }

  return (
    <section className="hub" aria-labelledby="hub-h">
      <div className="hub-head">
        <h3 id="hub-h" ref={headingRef} tabIndex={-1} className="card-subhead">
          Add a model from Hugging Face
        </h3>
        <Button size="sm" variant="ghost" onClick={onClose}>
          Done
        </Button>
      </div>
      <form
        className="hub-search"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          void search();
        }}
      >
        <TextField
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
                    <span className="hub-name">{r.id}</span>
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

type Pull = { done: number; total: number } | { error: string };

function RepoDetail({ api, repo, onBack }: { api: Api; repo: string; onBack: () => void }) {
  const toast = useToast();
  const [view, setView] = useState<HubRepoView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pulls, setPulls] = useState<Record<string, Pull>>({});
  const [done, setDone] = useState<Record<string, string>>({});
  const headingRef = useRef<HTMLHeadingElement>(null);

  const load = async () => {
    setError(null);
    setView(null);
    try {
      setView(await api.hubRepo(repo));
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  useEffect(() => {
    void load();
    headingRef.current?.focus();
    // Load once per repo.
  }, [repo]);

  const download = async (v: HubVariant) => {
    if (view === null) return;
    setPulls((p) => ({ ...p, [v.name]: { done: 0, total: v.total_size } }));
    try {
      const ref = await api.hubPull(view.id, view.sha, v.name, (ev) =>
        setPulls((p) => ({ ...p, [v.name]: { done: ev.done_bytes, total: ev.total_bytes } })),
      );
      setDone((d) => ({ ...d, [v.name]: ref }));
      setPulls(({ [v.name]: _, ...rest }) => rest);
      toast({ tone: "success", title: `${repo.split("/")[1] ?? repo} ${v.quant ?? v.name} downloaded` });
    } catch (e) {
      setPulls((p) => ({ ...p, [v.name]: { error: errorMessage(e) } }));
    }
  };

  return (
    <section className="hub" aria-labelledby="hub-repo-h" aria-busy={view === null && error === null}>
      <Button size="sm" variant="ghost" icon={<ArrowLeft {...ICON} aria-hidden="true" />} onClick={onBack}>
        Back to results
      </Button>
      <h3 id="hub-repo-h" ref={headingRef} tabIndex={-1} className="card-subhead hub-repo-name">
        {repo}
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
          {view.license.warning !== null && (
            <InlineMessage tone={LICENSE_NOTE[view.license.warning].tone}>{LICENSE_NOTE[view.license.warning].text}</InlineMessage>
          )}
          {view.gated && <HfToken api={api} />}
          {view.variants.length === 0 ? (
            <p className="dn-help">This repo has no GGUF files Demios can run.</p>
          ) : (
            <ul className="hub-variants" aria-label="Versions">
              {view.variants.map((v) => {
                const pull = pulls[v.name];
                const ref = done[v.name] ?? v.downloaded;
                const fit = v.fit !== null ? FIT[v.fit.status] : null;
                const nameId = `hubv-${v.name}`;
                return (
                  <li key={v.name} className="hub-variant" data-testid="hub-variant">
                    <div className="row-main">
                      <span className="row-title" id={nameId}>
                        {v.quant ?? v.name}{" "}
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
                      {pull !== undefined && "done" in pull && (
                        <div className="dl-progress">
                          <div
                            className="dl-bar"
                            role="progressbar"
                            aria-labelledby={nameId}
                            aria-valuemin={0}
                            aria-valuemax={pull.total}
                            aria-valuenow={pull.done}
                            aria-valuetext={`${formatBytes(pull.done)} of ${formatBytes(pull.total)}`}
                          >
                            <span style={{ width: `${pull.total > 0 ? Math.min(100, (pull.done / pull.total) * 100) : 0}%` }} />
                          </div>
                          <span className="dn-help">
                            {formatBytes(pull.done)} of {formatBytes(pull.total)}
                          </span>
                        </div>
                      )}
                      {pull !== undefined && "error" in pull && <InlineMessage tone="error">{pull.error}</InlineMessage>}
                      {!v.verifiable && (
                        <span className="dn-help">No published checksum, so Demios won't download it.</span>
                      )}
                    </div>
                    <div className="row-actions local-actions">
                      {ref !== null && ref !== undefined ? (
                        <Badge tone="success">Downloaded</Badge>
                      ) : pull !== undefined && "done" in pull ? (
                        <Button
                          size="sm"
                          icon={<Pause {...ICON} aria-hidden="true" />}
                          onClick={() => void api.hubCancel(view.id, view.sha, v.name)}
                        >
                          Pause
                        </Button>
                      ) : (
                        <Button
                          size="sm"
                          variant="secondary"
                          disabled={!v.verifiable}
                          icon={<Download {...ICON} aria-hidden="true" />}
                          onClick={() => void download(v)}
                        >
                          {pull !== undefined ? "Retry" : "Download"}
                        </Button>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
          <p className="dn-help">
            Downloads go to your models folder, are checked against Hugging Face's published checksum, and resume if
            interrupted.
          </p>
        </>
      )}
    </section>
  );
}

/** The Hugging Face token, for models that need access. Kept in the system keychain. */
function HfToken({ api }: { api: Api }) {
  const toast = useToast();
  const [has, setHas] = useState<boolean | null>(null);
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void api
      .hfTokenStatus()
      .then(setHas)
      .catch(() => setHas(false));
  }, [api]);

  return (
    <div className="hub-token">
      <InlineMessage tone="info">
        This model needs access: accept its terms on the model card, then add a Hugging Face token (read access is
        enough). The token is kept in your system keychain and is sent only to Hugging Face.
      </InlineMessage>
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
