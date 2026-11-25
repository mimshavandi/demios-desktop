// Windows: the artifact pane's frame lives in a separate webview (ADR-0012, threat model R8).
// WebView2 puts Tauri's IPC object into every frame of the main webview, so an artifact iframe
// there could reach the app; the child webview (`apps/desktop/src/artifact_child.rs`) is named
// by no capability, so nothing in it can. This adapter takes the iframe's place in the sandbox
// view (`FrameLike`):
// - the document is built and nonce'd here, exactly as for the iframe, and handed to Rust;
// - messages from the frame come back through Rust and are validated again here (nonce, fields);
// - the child is moved over this placeholder whenever the layout changes, and hidden while the
//   placeholder is off screen or an app overlay (dialog, menu, toast, tooltip) is over it;
// - focus: the placeholder passes focus into the child; Tab out of the child comes back as an
//   event and focus moves to the next (or previous) control here.
// The pane's labels, issue bar and live announcements stay in this (main) webview.

import { Channel } from "@tauri-apps/api/core";
import type { ArtifactFrameOptions, ArtifactType, FrameLike } from "@dn-artifacts";
import { api, type ChildEvent } from "./ipc/commands";
import { overlayOver, watchLayout, type Box } from "./overlay";

type Sandbox = typeof import("@dn-artifacts");

const FOCUSABLE = 'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

function randomToken(): string {
  const b = new Uint8Array(32);
  crypto.getRandomValues(b);
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}

/**
 * `rgb(…)` / `rgba(…)` → `#rrggbb`: the host page's background, the pane's surface colour, so it
 * doesn't flash. `null` when the colour isn't an rgb() value (the host page stays transparent).
 */
export function cssColorToHex(c: string): string | null {
  const m = /rgba?\((\d+),\s*(\d+),\s*(\d+)/.exec(c);
  if (!m) return null;
  return `#${[m[1], m[2], m[3]].map((v) => Number(v).toString(16).padStart(2, "0")).join("")}`;
}

/** The part of `el` that is on screen: its box clipped by every scrolling ancestor and the window. */
export function visibleBox(el: HTMLElement): Box | null {
  if (!el.isConnected || el.closest("[hidden]") !== null) return null;
  const r = el.getBoundingClientRect();
  let box: Box = { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
  for (let p = el.parentElement; p !== null; p = p.parentElement) {
    const s = getComputedStyle(p);
    if (s.display === "none" || s.visibility === "hidden") return null;
    if (/(auto|scroll|hidden|clip)/.test(s.overflow + s.overflowX + s.overflowY)) {
      const q = p.getBoundingClientRect();
      box = { left: Math.max(box.left, q.left), top: Math.max(box.top, q.top), right: Math.min(box.right, q.right), bottom: Math.min(box.bottom, q.bottom) };
    }
  }
  box = { left: Math.max(box.left, 0), top: Math.max(box.top, 0), right: Math.min(box.right, window.innerWidth), bottom: Math.min(box.bottom, window.innerHeight) };
  return box.right - box.left >= 1 && box.bottom - box.top >= 1 ? box : null;
}

export class ArtifactWebviewFrame implements FrameLike {
  readonly element: HTMLDivElement;
  readonly frame = null;
  private seq = 0;
  private nonce = "";
  private dead = false;
  private last = "";
  private readonly stop: () => void;
  private readonly opened: Promise<unknown>;

  constructor(
    private readonly m: Sandbox,
    container: HTMLElement,
    private readonly opts: ArtifactFrameOptions,
  ) {
    this.element = document.createElement("div");
    this.element.className = "dn-artifact-webview";
    this.element.tabIndex = 0;
    this.element.setAttribute("role", "region");
    this.element.setAttribute("aria-label", "Artifact content");
    this.element.setAttribute("data-testid", "artifact-webview-slot");
    container.appendChild(this.element);
    // Focus goes on into the child. The placeholder lets go of it, so the main webview doesn't
    // hand it straight back here when focus returns from the child.
    this.element.addEventListener("focus", () => {
      this.element.blur();
      void api.artifactChildFocus().catch(() => {});
    });

    const events = new Channel<ChildEvent>();
    events.onmessage = (ev) => this.handle(ev);
    const background = cssColorToHex(getComputedStyle(this.element).backgroundColor);
    this.opened = api.artifactChildOpen(randomToken(), background, events).catch((e: unknown) => {
      this.opts.onIssue?.({ kind: "render", message: "The artifact viewer couldn't open." });
      console.error("artifact webview", e);
    });
    this.stop = watchLayout(() => this.sync());
  }

  async render(type: ArtifactType, content: string, opts: { allowNetwork?: boolean; title?: string } = {}): Promise<void> {
    const seq = ++this.seq;
    const { html, nonce } = await this.m.prepareDocument(type, content, { allowNetwork: opts.allowNetwork === true });
    await this.opened;
    if (seq !== this.seq || this.dead) return; // superseded while loading
    this.nonce = nonce;
    await api.artifactChildRender({ seq, html, nonce, net: opts.allowNetwork === true, title: opts.title ?? "" });
  }

  destroy(): void {
    this.dead = true;
    this.stop();
    this.element.remove();
    void api.artifactChildClose().catch(() => {});
  }

  /** Moves the child over the placeholder, or hides it. Sends only changes. */
  private sync(): void {
    if (this.dead) return;
    const box = visibleBox(this.element);
    const show = box !== null && !overlayOver(box);
    const bounds = show ? { x: box.left, y: box.top, width: box.right - box.left, height: box.bottom - box.top } : null;
    const key = JSON.stringify(bounds && Object.fromEntries(Object.entries(bounds).map(([k, v]) => [k, Math.round(v * 2) / 2])));
    if (key === this.last) return;
    this.last = key;
    void this.opened.then(() => api.artifactChildBounds(bounds)).catch(() => {});
  }

  private handle(ev: ChildEvent): void {
    if (this.dead) return;
    switch (ev.kind) {
      case "frame": {
        if (ev.seq !== this.seq) return; // an earlier render
        const msg = this.m.parseFrameMessage(ev.data, this.nonce);
        if (!msg) return;
        if (msg.type === "ready") this.opts.onReady?.();
        else if (msg.type === "size") this.opts.onSize?.({ width: msg.width, height: msg.height });
        else if (msg.type === "issue") this.opts.onIssue?.(msg.issue);
        else if (msg.type === "key") this.opts.onKey?.(msg.key);
        return;
      }
      case "refused":
        if (ev.seq === this.seq) this.opts.onIssue?.({ kind: "render", message: "This artifact can't be shown safely in this window." });
        return;
      case "focus-out":
        this.focusAway(ev.dir);
        return;
    }
  }

  /** Focus the control after (or before) the placeholder, as Tab would have. */
  private focusAway(dir: "prev" | "next"): void {
    const all = [...document.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((e) => e === this.element || e.getClientRects().length > 0);
    const i = all.indexOf(this.element);
    const target = all[dir === "next" ? i + 1 : i - 1] ?? all[dir === "next" ? 0 : all.length - 1];
    target?.focus();
  }
}
