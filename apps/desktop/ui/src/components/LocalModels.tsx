// "On this device" card (T-SHELL-07): curated catalogue models with download state and
// fit status. Download row states follow design-system §4: queued, downloading (progress
// bar with size + ETA, Pause), paused, verifying, ready, failed (reason + Retry). Licence and
// model card are shown before download, with a warning for missing / non-commercial /
// custom licences (any licence is allowed).

import { Copy, Cpu, Download, Pause, Play, Plus, RotateCcw, TriangleAlert } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import { errorMessage, type AddModelLink, type FitStatus, type LocalModelRow } from "../ipc/types";
import { Badge, Button, ICON, IconButton, InlineMessage } from "./primitives";
import { useToast } from "./toast";
import { AddFromHub } from "./AddFromHub";

export const SMALL = { size: 12, strokeWidth: 1.5 } as const;

export function formatBytes(b: number): string {
  if (b >= 1e9) return `${(b / 1e9).toFixed(1)} GB`;
  if (b >= 1e6) return `${Math.round(b / 1e6)} MB`;
  return `${Math.max(1, Math.round(b / 1e3))} KB`;
}

export function formatEta(remaining: number, bytesPerSec: number): string | null {
  if (bytesPerSec <= 0) return null;
  const s = Math.round(remaining / bytesPerSec);
  if (s < 60) return "less than a minute left";
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min left`;
  return `${Math.floor(m / 60)} h ${m % 60} min left`;
}

export const FIT: Record<FitStatus, { label: string; tone: "success" | "warning" | "danger" }> = {
  ready: { label: "Ready", tone: "success" },
  caution: { label: "Caution", tone: "warning" },
  memory_pressure: { label: "Memory pressure", tone: "warning" },
  memory_blocked: { label: "Won't fit", tone: "danger" },
};

export function LocalModelsCard({
  api,
  initial,
  onReady,
  link = null,
  onLinkClosed,
}: {
  api: Api;
  initial: LocalModelRow[];
  /** A model finished downloading: refresh pickers and the composer state. */
  onReady: () => void;
  /** A demios:// link to confirm (opens the add panel on it). */
  link?: AddModelLink | null;
  onLinkClosed?: () => void;
}) {
  const [rows, setRows] = useState<LocalModelRow[]>(initial);
  const [adding, setAdding] = useState(false);
  const addRef = useRef<HTMLButtonElement>(null);
  const [announce, setAnnounce] = useState("");
  // Following downloads failed: say so (with Retry) rather than show frozen rows (#141).
  const [watchError, setWatchError] = useState<string | null>(null);
  const [watchTry, setWatchTry] = useState(0);
  // Rows updated by events before the snapshot arrived: the snapshot must not undo them.
  const fresh = useRef<Set<string>>(new Set());
  const prev = useRef<Map<string, string>>(new Map(initial.map((r) => [r.id, r.state.state])));

  useEffect(() => {
    let alive = true;
    setWatchError(null);
    void api
      .watchLocalModels((ev) => {
        if (!alive) return;
        if (ev.type === "removed") {
          setRows((list) => list.filter((r) => r.id !== ev.id));
          prev.current.delete(ev.id);
          return;
        }
        fresh.current.add(ev.row.id);
        // Upsert: Hub downloads and cache models arrive as new rows.
        setRows((list) => (list.some((r) => r.id === ev.row.id) ? list.map((r) => (r.id === ev.row.id ? ev.row : r)) : [...list, ev.row]));
        const was = prev.current.get(ev.row.id);
        prev.current.set(ev.row.id, ev.row.state.state);
        if (ev.row.state.state === "ready" && was !== "ready") {
          onReady();
          // Polite, once per finished download (design-system §7).
          setAnnounce("");
          window.setTimeout(() => setAnnounce(`${ev.row.name} downloaded`), 50);
        }
      })
      .then((list) => {
        if (!alive) return;
        setRows((current) =>
          list.map((r) => (fresh.current.has(r.id) ? (current.find((c) => c.id === r.id) ?? r) : r)),
        );
        for (const r of list) if (!fresh.current.has(r.id)) prev.current.set(r.id, r.state.state);
      })
      .catch((e: unknown) => {
        if (alive) setWatchError(errorMessage(e));
      });
    return () => {
      alive = false;
    };
  }, [api, onReady, watchTry]);

  return (
    <section className="card" aria-labelledby="local-h">
      <div className="card-head">
        <h2 id="local-h">On this device</h2>
        {!adding && link === null && (
          <Button ref={addRef} size="sm" icon={<Plus {...ICON} aria-hidden="true" />} onClick={() => setAdding(true)}>
            Add from Hugging Face
          </Button>
        )}
      </div>
      <p className="dn-help">
        Runs on this computer; nothing leaves it. Downloads resume if interrupted and are checked
        against their published checksum.
      </p>
      {link !== null && (
        <AddFromHub
          api={api}
          link={link}
          onClose={() => {
            onLinkClosed?.();
            requestAnimationFrame(() => addRef.current?.focus());
          }}
        />
      )}
      {adding && link === null && (
        <AddFromHub
          api={api}
          onClose={() => {
            setAdding(false);
            requestAnimationFrame(() => addRef.current?.focus());
          }}
        />
      )}
      {watchError !== null && (
        <InlineMessage
          tone="error"
          action={
            <Button size="sm" onClick={() => setWatchTry((n) => n + 1)}>
              Retry
            </Button>
          }
        >
          Couldn't load the models on this device, so downloads may look out of date. {watchError}
        </InlineMessage>
      )}
      {watchError === null && rows.length === 0 && <p className="dn-help">No models in the catalogue.</p>}
      {rows.map((r) => (
        <LocalModelRowView key={r.id} api={api} row={r} />
      ))}
      <div className="dn-visually-hidden" role="status" aria-live="polite">
        {announce}
      </div>
    </section>
  );
}

function LocalModelRowView({ api, row }: { api: Api; row: LocalModelRow }) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const s = row.state;
  const fit = FIT[row.fit.status];
  const nameId = `local-${row.id}`;

  const act = async (f: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await f();
    } catch (e) {
      toast({ tone: "error", title: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };
  const download = () => act(() => api.downloadModel(row.id));
  const pause = () => act(() => api.pauseDownload(row.id));

  const progress = (done: number, total: number, label: string) => (
    <div className="dl-progress">
      <div
        className="dl-bar"
        role="progressbar"
        aria-labelledby={nameId}
        aria-valuemin={0}
        aria-valuemax={total}
        aria-valuenow={done}
        aria-valuetext={label}
      >
        <span style={{ width: `${total > 0 ? Math.min(100, (done / total) * 100) : 0}%` }} />
      </div>
      <span className="dn-help">{label}</span>
    </div>
  );

  return (
    <div className="row local-row" data-testid="local-model-row" data-state={s.state}>
      <div className="row-main">
        <span className="row-title" id={nameId}>
          {row.name} <span className="dn-help">{row.quant} · {formatBytes(row.size)}</span>
        </span>
        {row.origin !== "catalog" && (
          <span className="dn-help local-origin">
            {row.origin === "hub" ? "From Hugging Face" : "From your Hugging Face cache, used where it is"}
            {row.location !== null ? (
              <>
                {" · "}
                <code>{row.location}</code>
              </>
            ) : null}
          </span>
        )}
        <span className="local-meta">
          <Badge
            tone={fit.tone}
            icon={<Cpu {...SMALL} aria-hidden="true" />}
          >
            {fit.label}
            {row.fit.estimated ? " (estimate)" : ""}
          </Badge>
          <span className="dn-help">{row.fit.reason}</span>
        </span>
        <span className="dn-help local-license">
          Licence: {row.license === "" ? "not stated" : row.license} · Model card:{" "}
          <code>{row.card_url.replace(/^https:\/\//, "")}</code>
          <IconButton
            size="sm"
            label="Copy model card link"
            onClick={() =>
              void navigator.clipboard
                ?.writeText(row.card_url)
                .then(() => toast({ tone: "success", title: "Link copied" }))
            }
          >
            <Copy {...ICON} aria-hidden="true" />
          </IconButton>
        </span>
        {row.license_warning && s.state !== "ready" && (
          <InlineMessage tone="warning">
            Check the licence before downloading: it is missing, non-commercial or custom.
          </InlineMessage>
        )}
        {s.state === "downloading" &&
          progress(
            s.done,
            s.total,
            [`${formatBytes(s.done)} of ${formatBytes(s.total)}`, formatEta(s.total - s.done, s.bytes_per_sec)]
              .filter(Boolean)
              .join(" · "),
          )}
        {s.state === "paused" && progress(s.done, s.total, `Paused at ${formatBytes(s.done)} of ${formatBytes(s.total)}`)}
        {s.state === "verifying" && <span className="dn-help">Checking the file…</span>}
        {s.state === "queued" && (
          <span className="dn-help">{s.resuming ? "Checking the partial download…" : "Starting…"}</span>
        )}
        {s.state === "failed" && (
          <InlineMessage tone="error">{s.message}</InlineMessage>
        )}
      </div>
      <div className="row-actions local-actions">
        {s.state === "not_downloaded" && (
          <Button
            icon={<Download {...ICON} aria-hidden="true" />}
            loading={busy}
            onClick={() => void download()}
            aria-describedby={nameId}
          >
            Download model
          </Button>
        )}
        {(s.state === "downloading" || s.state === "queued") && (
          <Button icon={<Pause {...ICON} aria-hidden="true" />} loading={busy} onClick={() => void pause()}>
            Pause
          </Button>
        )}
        {s.state === "paused" && (
          <Button icon={<Play {...ICON} aria-hidden="true" />} loading={busy} onClick={() => void download()}>
            Resume
          </Button>
        )}
        {s.state === "failed" && (
          <Button icon={<RotateCcw {...ICON} aria-hidden="true" />} loading={busy} onClick={() => void download()}>
            Retry
          </Button>
        )}
        {s.state === "ready" && (
          <Badge tone="local" icon={<Cpu {...SMALL} aria-hidden="true" />}>
            On this device
          </Badge>
        )}
        {row.fit.status === "memory_blocked" && s.state === "not_downloaded" && (
          <span className="dn-help local-blocked">
            <TriangleAlert {...SMALL} aria-hidden="true" /> Likely too large
          </span>
        )}
      </div>
    </div>
  );
}
