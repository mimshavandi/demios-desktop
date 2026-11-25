// The embedding-model offer (#171). The model isn't bundled: it is offered once, the first time
// a project has files and no embedding model is installed ("Not now" is remembered and never
// asked again), and always in Settings › Models. On "Download", the normal verified download
// runs, then the project's existing passages are embedded in the background, from the stored
// chunks (no file is read again). Keyword search keeps answering meanwhile.

import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import type { KnowledgeStatus } from "../ipc/types";
import { formatBytes } from "./LocalModels";
import { Button, InlineMessage } from "./primitives";

/** How often the status is read again while something is in progress. */
const POLL_MS = 1500;

/** The offer's question, with the model's download size. */
export function offerText(size: number): string {
  return `Turn on better search? Downloads the ${formatBytes(size)} search model once.`;
}

/** What is going on, for a progress line: downloading, improving search, or nothing. */
export function progressText(s: KnowledgeStatus | null): string | null {
  const st = s?.model?.state;
  if (st?.state === "downloading" || st?.state === "paused") {
    return `Downloading the search model: ${formatBytes(st.done)} of ${formatBytes(st.total)}.`;
  }
  if (st?.state === "queued" || st?.state === "verifying") return "Downloading the search model…";
  if (s?.embedding) return `Improving search: ${s.embedding.done} of ${s.embedding.total} passages.`;
  return null;
}

const busy = (s: KnowledgeStatus | null) => progressText(s) !== null;

/** Reads the status, and again every POLL_MS while a download or embedding is running. */
export function useKnowledgeStatus(load: () => Promise<KnowledgeStatus>) {
  const [status, setStatus] = useState<KnowledgeStatus | null>(null);
  const [failed, setFailed] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  const refresh = useCallback(async () => {
    window.clearTimeout(timer.current);
    try {
      const s = await load();
      setStatus(s);
      setFailed(false);
      if (busy(s)) timer.current = window.setTimeout(() => void refresh(), POLL_MS);
    } catch (e) {
      console.error("knowledge status", e);
      setFailed(true);
    }
  }, [load]);
  useEffect(() => {
    void refresh();
    return () => window.clearTimeout(timer.current);
  }, [refresh]);
  return { status, failed, refresh };
}

/** The one-time prompt in a project's Files card (only while the project has files). */
export function BetterSearchOffer({ api, hasFiles }: { api: Api; hasFiles: boolean }) {
  const { status, refresh } = useKnowledgeStatus(api.knowledgeStatus);
  const [error, setError] = useState<string | null>(null);
  const [answered, setAnswered] = useState(false);
  const answer = async (accept: boolean) => {
    setError(null);
    try {
      await api.knowledgeOffer(accept);
      setAnswered(true);
      await refresh();
    } catch (e) {
      setError(accept ? "The search model couldn't be downloaded. Try again." : "Your choice couldn't be saved.");
      console.error("knowledge offer", e);
    }
  };
  if (!hasFiles || status?.model == null) return null;
  const progress = progressText(status);
  if (answered && progress !== null) {
    return (
      <p className="dn-help" role="status" data-testid="better-search-progress">
        {progress}
      </p>
    );
  }
  const notInstalled = status.model.state.state === "not_downloaded" || status.model.state.state === "failed";
  if (answered || status.offer !== "ask" || !notInstalled) return null;
  return (
    <div data-testid="better-search-offer">
      <InlineMessage
        tone="info"
        action={
          <span className="row-actions">
            <Button size="sm" onClick={() => void answer(true)} data-testid="better-search-download">
              Download
            </Button>
            <Button size="sm" onClick={() => void answer(false)} data-testid="better-search-not-now">
              Not now
            </Button>
          </span>
        }
      >
        {offerText(status.model.size)}
      </InlineMessage>
      {error !== null && <InlineMessage tone="error">{error}</InlineMessage>}
    </div>
  );
}

/** The same choice in Settings › Models, always available (also after "Not now"). */
export function BetterSearchSetting({ api }: { api: Api }) {
  const { status, failed, refresh } = useKnowledgeStatus(api.knowledgeStatus);
  const [error, setError] = useState<string | null>(null);
  if (failed) return <InlineMessage tone="error">Project search status couldn't be loaded.</InlineMessage>;
  if (status?.model == null) return null;
  const st = status.model.state.state;
  const progress = progressText(status);
  const download = async () => {
    setError(null);
    try {
      await api.knowledgeOffer(true);
      await refresh();
    } catch (e) {
      console.error("knowledge offer", e);
      setError("The search model couldn't be downloaded. Try again.");
    }
  };
  return (
    <section className="card" aria-labelledby="better-search-h" data-testid="better-search-setting">
      <h2 id="better-search-h">Project search</h2>
      {st === "ready" && progress === null ? (
        <p className="dn-help">Searching project files uses {status.model.name}: keywords and meaning.</p>
      ) : progress !== null ? (
        <p className="dn-help" role="status">
          {progress} Keyword search works meanwhile.
        </p>
      ) : (
        <div className="row-actions">
          <p className="dn-help">
            Project files are searched by keywords. Download the {formatBytes(status.model.size)} search model to also
            find passages by meaning.
          </p>
          <Button size="sm" onClick={() => void download()} data-testid="better-search-setting-download">
            {st === "failed" ? "Try again" : "Download"}
          </Button>
        </div>
      )}
      {error !== null && <InlineMessage tone="error">{error}</InlineMessage>}
    </section>
  );
}
