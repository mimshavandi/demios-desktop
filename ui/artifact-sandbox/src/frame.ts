// The sandboxed frame. Security-relevant choices (see docs/security/artifact-sandbox.md):
// - The frame loads the shell page from a separate artifact origin (ADR-0004 `artifact://`),
//   served with the artifact CSP as a header. (A `srcdoc` frame would inherit the host's CSP,
//   and a strict host CSP would then block the artifact's own scripts.)
// - `sandbox="allow-scripts"` and never `allow-same-origin`: the frame gets an opaque origin, so
//   it cannot touch the host DOM, cookies, storage or the host's JS globals (incl. Tauri IPC).
// - No allow-top-navigation / allow-popups / allow-forms / allow-modals / allow-downloads.
// - Content is posted in exactly once per frame; a new version means a new frame (fresh realm).
//   A frame that reloads or navigates itself never gets the content again.
// - Messages from the frame are accepted only from that frame's window and current nonce, and
//   are treated as untrusted data (parseFrameMessage).

import { isSeparateOrigin } from "./origin";
import { CHANNEL, parseFrameMessage } from "./protocol";
import { runtimesFor } from "./runtimes";
import { buildDocument, rendererFor } from "./document";
import type { ArtifactType, SandboxIssue } from "./types";

export const SANDBOX = "allow-scripts";


export interface ArtifactFrameOptions {
  /**
   * URL of the frame shell (`dist/frame/index.html`) on the artifact origin, e.g.
   * `artifact://localhost/frame/` in the desktop app. Must not be the host's own origin.
   */
  frameUrl: string;
  /** Grow the frame to its content height (reported by the frame), within [minHeight, maxHeight]. */
  autoHeight?: boolean;
  minHeight?: number;
  maxHeight?: number;
  onIssue?: (issue: SandboxIssue) => void;
  onReady?: () => void;
  onSize?: (size: { width: number; height: number }) => void;
}

export class ArtifactFrame {
  readonly element: HTMLDivElement;
  private iframe: HTMLIFrameElement | null = null;
  private nonce = "";
  private renderSeq = 0;
  /** Content waiting for the current frame's shell; cleared once sent (sent at most once). */
  private pending: { html: string; resolve: () => void } | null = null;
  private readonly opts: ArtifactFrameOptions;
  private readonly onMessage = (e: MessageEvent): void => this.handle(e);

  constructor(container: HTMLElement, opts: ArtifactFrameOptions) {
    if (!isSeparateOrigin(opts.frameUrl, location.href)) {
      throw new Error("artifact frameUrl must be on a separate origin from the host");
    }
    this.opts = opts;
    this.element = document.createElement("div");
    this.element.className = "dn-artifact-frame";
    container.appendChild(this.element);
    window.addEventListener("message", this.onMessage);
  }

  /** Render content in a brand-new frame. Resolves once the content has been posted to it. */
  async render(type: ArtifactType, content: string, opts: { allowNetwork?: boolean; title?: string } = {}): Promise<void> {
    const seq = ++this.renderSeq;
    const runtimes = await runtimesFor(rendererFor(type));
    if (seq !== this.renderSeq) return; // superseded while loading
    const nonce = randomNonce();
    const allowNetwork = opts.allowNetwork === true;
    const html = buildDocument(type, content, runtimes, { nonce, allowNetwork });

    const url = new URL(this.opts.frameUrl, location.href);
    if (allowNetwork) url.searchParams.set("net", "1");

    const iframe = document.createElement("iframe");
    // Attributes must be set before the frame is inserted (the sandbox applies at navigation).
    iframe.setAttribute("sandbox", SANDBOX);
    iframe.setAttribute("referrerpolicy", "no-referrer");
    iframe.setAttribute("allow", "");
    iframe.setAttribute("title", opts.title ? `Artifact: ${opts.title}` : "Artifact");
    iframe.setAttribute("data-testid", "artifact-frame");
    iframe.style.cssText = `display:block;width:100%;border:0;height:${this.opts.minHeight ?? 160}px`;

    this.pending?.resolve();
    const posted = new Promise<void>((resolve) => {
      this.pending = { html, resolve };
    });
    iframe.src = url.href;

    this.iframe?.remove();
    this.iframe = iframe;
    this.nonce = nonce;
    this.element.appendChild(iframe);
    await posted;
  }

  /** The current iframe (tests and embedding code; never script into it). */
  get frame(): HTMLIFrameElement | null {
    return this.iframe;
  }

  destroy(): void {
    window.removeEventListener("message", this.onMessage);
    this.pending?.resolve();
    this.pending = null;
    this.iframe?.remove();
    this.iframe = null;
    this.element.remove();
  }

  private handle(e: MessageEvent): void {
    const iframe = this.iframe;
    // The frame is sandboxed without allow-same-origin, so its messages always come from the
    // opaque origin "null"; anything else is not our frame.
    if (!iframe || e.source !== iframe.contentWindow || e.origin !== "null") return;
    const d = e.data as { channel?: unknown; type?: unknown } | null;
    if (d && d.channel === CHANNEL && d.type === "shell-ready") {
      // The shell asks for its content. Answer once per frame: a frame that navigated or
      // reloaded itself (and so lost its content) stays empty.
      const p = this.pending;
      this.pending = null;
      if (p) {
        iframe.contentWindow?.postMessage({ channel: CHANNEL, type: "render", html: p.html }, "*");
        p.resolve();
      }
      return;
    }
    const msg = parseFrameMessage(e.data, this.nonce);
    if (!msg) return;
    switch (msg.type) {
      case "ready":
        this.opts.onReady?.();
        break;
      case "size": {
        if (this.opts.autoHeight !== false) {
          const min = this.opts.minHeight ?? 160;
          const max = this.opts.maxHeight ?? 2000;
          iframe.style.height = `${Math.min(max, Math.max(min, msg.height))}px`;
        }
        this.opts.onSize?.({ width: msg.width, height: msg.height });
        break;
      }
      case "issue":
        this.opts.onIssue?.(msg.issue);
        break;
    }
  }
}

function randomNonce(): string {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}
