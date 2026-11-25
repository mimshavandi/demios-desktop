// Hugging Face downloads started from the add-model dialog (#174). They run here, outside any
// component, so leaving Settings › Models (which unmounts the list) doesn't lose their progress;
// the On this device list subscribes and shows them until they finish.

import { useSyncExternalStore } from "react";
import type { Api } from "./ipc/commands";
import { errorMessage } from "./ipc/types";

/** A version the user chose to download. */
export interface HubDownload {
  repo: string;
  sha: string;
  variant: string;
  /** "Tiny-GGUF Q4_K_M" */
  label: string;
  total: number;
}

export type HubRun =
  | { kind: "running"; d: HubDownload; done: number; total: number }
  | { kind: "paused"; d: HubDownload; done: number; total: number }
  | { kind: "failed"; d: HubDownload; error: string };

export const downloadKey = (d: { repo: string; sha: string; variant: string }) => `${d.repo}@${d.sha}:${d.variant}`;

type Finished = (d: HubDownload) => void;

let runs: Record<string, HubRun> = {};
const listeners = new Set<() => void>();
const finishedListeners = new Set<Finished>();

function set(next: Record<string, HubRun>) {
  runs = next;
  for (const l of listeners) l();
}

/** Starts (or resumes) a download. Progress, pause and failure end up in the store. */
export function startHubDownload(api: Api, d: HubDownload, from = 0): void {
  const key = downloadKey(d);
  if (runs[key]?.kind === "running") return;
  set({ ...runs, [key]: { kind: "running", d, done: from, total: d.total } });
  void api
    .hubPull(d.repo, d.sha, d.variant, (ev) => {
      if (runs[key]?.kind !== "running") return;
      set({ ...runs, [key]: { kind: "running", d, done: ev.done_bytes, total: ev.total_bytes || d.total } });
    })
    .then(() => {
      const { [key]: _done, ...rest } = runs;
      set(rest);
      for (const f of finishedListeners) f(d);
    })
    .catch((e: unknown) => {
      const paused = typeof e === "object" && e !== null && (e as { code?: string }).code === "cancelled";
      const cur = runs[key];
      const at = cur !== undefined && cur.kind !== "failed" ? cur : { done: 0, total: d.total };
      set({
        ...runs,
        [key]: paused ? { kind: "paused", d, done: at.done, total: at.total } : { kind: "failed", d, error: errorMessage(e) },
      });
    });
}

/** Asks the backend to pause; the run turns "paused" when hub_pull returns. */
export function pauseHubDownload(api: Api, d: HubDownload): Promise<unknown> {
  return api.hubCancel(d.repo, d.sha, d.variant);
}

/** Called when a download finishes (from any list or none). Returns the unsubscribe. */
export function onHubDownloadFinished(f: Finished): () => void {
  finishedListeners.add(f);
  return () => finishedListeners.delete(f);
}

export function useHubDownloads(): Record<string, HubRun> {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => runs,
  );
}

/** Tests only: forget every run. */
export function resetHubDownloads(): void {
  set({});
}
