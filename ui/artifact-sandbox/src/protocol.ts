// postMessage protocol between the sandboxed frame and the host.
// Handshake: the frame shell posts `shell-ready`; the host answers once with `render` (the full
// artifact document). After that, messages only go frame → host (ready, size, issue). A shell that
// finds host IPC objects it can't remove posts `shell-refused` instead and never runs artifact code.

import type { SandboxIssue } from "./types";

export const CHANNEL = "dn-artifact";

export type FrameMessage =
  | { type: "ready" }
  | { type: "size"; width: number; height: number }
  | { type: "issue"; issue: SandboxIssue }
  | { type: "key"; key: KeyPress };

/**
 * A key pressed inside the frame that the app may act on: Escape, or a combination with Ctrl or
 * ⌘ (app shortcuts). Keys typed inside an artifact otherwise never leave the frame. Artifact code
 * can forge these messages, so the host acts on them only as it would on the user's own keys,
 * and only on its own allowlist.
 */
export interface KeyPress {
  key: string;
  ctrl: boolean;
  meta: boolean;
  alt: boolean;
  shift: boolean;
}

/** Whether a key press may leave the frame (bridge.ts) or be acted on by the host. */
export function isForwardedKey(k: KeyPress): boolean {
  return k.key === "Escape" || ((k.ctrl || k.meta) && k.key.length > 0);
}

const MAX_TEXT = 4000;
const MAX_PX = 100_000;

const clampText = (v: unknown): string | undefined =>
  typeof v === "string" ? v.slice(0, MAX_TEXT) : undefined;

const ISSUE_KINDS: ReadonlySet<string> = new Set(["error", "unhandledrejection", "console", "csp", "network", "render"]);

/**
 * Validate a message that claims to come from the frame. Everything in it is untrusted data
 * written by artifact code: only plain, length-limited fields survive.
 */
export function parseFrameMessage(data: unknown, nonce: string): FrameMessage | null {
  if (typeof data !== "object" || data === null) return null;
  const d = data as Record<string, unknown>;
  if (d["channel"] !== CHANNEL || d["nonce"] !== nonce) return null;
  switch (d["type"]) {
    case "ready":
      return { type: "ready" };
    case "size": {
      const w = d["width"];
      const h = d["height"];
      if (typeof w !== "number" || typeof h !== "number" || !Number.isFinite(w) || !Number.isFinite(h)) return null;
      return { type: "size", width: clamp(w), height: clamp(h) };
    }
    case "issue": {
      const i = d["issue"];
      if (typeof i !== "object" || i === null) return null;
      const r = i as Record<string, unknown>;
      const kind = r["kind"];
      const message = clampText(r["message"]);
      if (typeof kind !== "string" || !ISSUE_KINDS.has(kind) || message === undefined) return null;
      const detail = clampText(r["detail"]);
      const issue: SandboxIssue = { kind: kind as SandboxIssue["kind"], message };
      if (detail) issue.detail = detail;
      return { type: "issue", issue };
    }
    case "key": {
      const k = d["key"];
      if (typeof k !== "object" || k === null) return null;
      const r = k as Record<string, unknown>;
      const key = r["key"];
      const flag = (n: string): boolean | undefined => (typeof r[n] === "boolean" ? (r[n] as boolean) : undefined);
      const [ctrl, meta, alt, shift] = [flag("ctrl"), flag("meta"), flag("alt"), flag("shift")];
      if (typeof key !== "string" || key.length === 0 || key.length > 32) return null;
      if (ctrl === undefined || meta === undefined || alt === undefined || shift === undefined) return null;
      const press: KeyPress = { key, ctrl, meta, alt, shift };
      return isForwardedKey(press) ? { type: "key", key: press } : null;
    }
    default:
      return null;
  }
}

function clamp(px: number): number {
  return Math.min(MAX_PX, Math.max(0, Math.round(px)));
}
