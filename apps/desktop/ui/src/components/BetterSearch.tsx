// The embedding-model offer (#171). The model isn't bundled: it is offered once, the first time
// a project has files and no embedding model is installed ("Not now" is remembered and never
// asked again). In Settings › Models the search model is picked like any other model
// (Project search slot, Set as default). On "Download", the normal verified download runs, then the project's existing passages are embedded in the background, from the stored
// chunks (no file is read again). Keyword search keeps answering meanwhile.
//
// #201 made nomic-embed-text v2 the search model. Someone who accepted the earlier model (v1.5)
// is asked once more ("upgrade"), since the new one is a new download; its vectors can't be
// mixed with the old ones, so search is keyword only until then, and the passages are then
// re-embedded from the stored text.

import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import type { KnowledgeStatus, OfferedModel } from "../ipc/types";
import { formatBytes } from "./LocalModels";
import { Button, InlineMessage } from "./primitives";

/** How often the status is read again while something is in progress. */
const POLL_MS = 1500;

/** The offer's question, with the model's download size and licence. */
export function offerText(m: Pick<OfferedModel, "size" | "license">): string {
  return `Turn on better search? Downloads the ${formatBytes(m.size)} search model once. ${licenceText(m.license)}`;
}

/** The question for someone who turned on the earlier search model (offer "upgrade"). */
export function upgradeText(m: Pick<OfferedModel, "size" | "license">): string {
  return (
    `A better search model is available: it also finds passages in other languages. Download it once ` +
    `(${formatBytes(m.size)}), and your projects' passages are re-indexed with it from the text already stored; ` +
    `no files are read again. Until then, search uses keywords only. ${licenceText(m.license)}`
  );
}

export function licenceText(license: string): string {
  return `Licence: ${license === "" ? "not stated" : license}.`;
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
  const asking = status.offer === "ask" || status.offer === "upgrade";
  if (answered || !asking || !notInstalled) return null;
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
        {status.offer === "upgrade" ? upgradeText(status.model) : offerText(status.model)}
      </InlineMessage>
      {error !== null && <InlineMessage tone="error">{error}</InlineMessage>}
    </div>
  );
}
