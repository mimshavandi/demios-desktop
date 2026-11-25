// What an answer in a project chat was given from the project's files (#116, knowledge.rs).
// The sources come from the retrieval itself, recorded when the turn started, not from what
// the model wrote, so a citation can't be invented. Shown under the answer: the file and line
// range of each excerpt, and, when no embedding model is installed, that the search was
// keyword-only and how to get better results (the reduced mode is labelled, never silent).

import { useCallback, useEffect, useState } from "react";
import type { Id, KnowledgeStatus, MessageSources } from "../ipc/types";
import { progressText } from "./BetterSearch";
import { Button, InlineMessage } from "./primitives";

type Loader = (messageId: Id) => Promise<MessageSources | null>;
type StatusLoader = () => Promise<KnowledgeStatus>;

let loader: Loader | null = null;
let statusLoader: StatusLoader | null = null;

/** Set once by the app with its IPC (the real one, or the browser mock). */
export function configureSources(load: Loader, status?: StatusLoader): void {
  loader = load;
  statusLoader = status ?? null;
}

export const KEYWORD_ONLY =
  "Keyword search. Add the embedding model in Settings › Models for better results.";

type State =
  | { kind: "loading" }
  | { kind: "none" }
  | { kind: "ready"; sources: MessageSources }
  | { kind: "error" };

export function Sources({ messageId }: { messageId: Id }) {
  const [state, setState] = useState<State>({ kind: "loading" });
  // Keyword-only answers: whether the search model is on its way (download or embedding).
  const [progress, setProgress] = useState<string | null>(null);
  const keyword = state.kind === "ready" && state.sources.mode === "keyword";
  useEffect(() => {
    if (!keyword || statusLoader === null) return;
    let live = true;
    statusLoader()
      .then((s) => {
        if (live) setProgress(progressText(s));
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [keyword]);
  const load = useCallback(() => {
    if (loader === null) {
      setState({ kind: "none" });
      return () => {};
    }
    let live = true;
    setState({ kind: "loading" });
    loader(messageId)
      .then((s) => {
        if (live) setState(s === null ? { kind: "none" } : { kind: "ready", sources: s });
      })
      .catch((e: unknown) => {
        console.error("sources", e);
        if (live) setState({ kind: "error" });
      });
    return () => {
      live = false;
    };
  }, [messageId]);
  useEffect(() => load(), [load]);

  switch (state.kind) {
    case "none":
      return null;
    case "loading":
      return (
        <div className="sources sources-loading" aria-busy="true" data-testid="sources-loading">
          Loading sources…
        </div>
      );
    case "error":
      return (
        <div className="sources" data-testid="sources-error">
          <InlineMessage
            tone="error"
            action={
              <Button size="sm" onClick={() => load()}>
                Try again
              </Button>
            }
          >
            The sources for this answer couldn't be loaded.
          </InlineMessage>
        </div>
      );
    case "ready": {
      const { citations, mode } = state.sources;
      return (
        <section className="sources" aria-label="Sources from this project's files" data-testid="sources">
          {citations.length === 0 ? (
            <p className="sources-empty" data-testid="sources-empty">
              No passages in this project's files matched this question.
            </p>
          ) : (
            <ol className="sources-list">
              {citations.map((c) => (
                <li key={c.label} data-testid="source">
                  <span className="sources-label">[{c.label}]</span> <span className="sources-file">{c.display_path}</span>{" "}
                  <span className="sources-lines">
                    {c.start_line === c.end_line ? `line ${c.start_line}` : `lines ${c.start_line}–${c.end_line}`}
                  </span>
                </li>
              ))}
            </ol>
          )}
          {mode === "keyword" && (
            <p className="sources-mode" data-testid="sources-keyword-only">
              {progress ?? KEYWORD_ONLY}
            </p>
          )}
        </section>
      );
    }
  }
}
