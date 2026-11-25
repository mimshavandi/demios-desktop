// Host page of the desktop app's artifact webview (Windows only; apps/desktop/src/artifact_child.rs,
// ADR-0012). WebView2 runs Tauri's IPC scripts in every frame and they can't be removed, so on
// Windows artifacts don't render in an iframe of the main window (which holds the app's
// capability). They render here instead: a separate webview, labelled `artifact`, that no
// capability names, so every IPC call from it (this page or the artifact frame) is refused.
//
// This page holds the same sandboxed iframe as ArtifactFrame (opaque origin, artifact CSP, one
// render per frame, nonce-checked messages). It talks to the app only through Rust:
// - Rust → page: `window.__dnArtifactHost.render(seq, token)` / `.focus(token)` (webview eval);
// - page → Rust: GET /host/doc (the document for a render) and POST /host/event (frame messages,
//   focus leaving), both with the per-session token in a header. The token is in this realm only:
//   the artifact frame is cross-origin and never sees it.
// Frame messages are validated here (source, nonce, fields) and again in the main window.

import { CHANNEL, parseFrameMessage } from "../protocol";
import { SANDBOX } from "../frame";

interface Doc {
  html: string;
  nonce: string;
  net: boolean;
  title: string;
}

const MIN_HEIGHT = 160;
const MAX_HEIGHT = 20_000;

let token = "";
let seq = 0;
let nonce = "";
let iframe: HTMLIFrameElement | null = null;
let pending: string | null = null;

const slot = (): HTMLElement => document.getElementById("dn-frame") as HTMLElement;

function send(body: unknown): void {
  if (!token) return;
  void fetch("/host/event", {
    method: "POST",
    headers: { "content-type": "application/json", "x-dn-token": token },
    body: JSON.stringify(body),
  }).catch(() => {
    /* the app is closing the webview */
  });
}

async function render(n: number, t: string): Promise<void> {
  token = t;
  const res = await fetch(`/host/doc?seq=${encodeURIComponent(String(n))}`, { headers: { "x-dn-token": token } });
  if (!res.ok) return;
  const doc = (await res.json()) as Doc;
  if (n < seq) return; // superseded while loading
  seq = n;
  nonce = doc.nonce;
  pending = doc.html;
  const f = document.createElement("iframe");
  // Attributes before insertion: the sandbox applies at navigation (as in ArtifactFrame).
  f.setAttribute("sandbox", SANDBOX);
  f.setAttribute("referrerpolicy", "no-referrer");
  f.setAttribute("allow", "");
  f.setAttribute("title", doc.title ? `Artifact: ${doc.title}` : "Artifact");
  f.setAttribute("data-testid", "artifact-frame");
  f.style.cssText = `display:block;width:100%;border:0;height:${Math.max(MIN_HEIGHT, window.innerHeight)}px`;
  f.src = doc.net ? "/frame/?net=1" : "/frame/";
  iframe?.remove();
  iframe = f;
  slot().appendChild(f);
}

window.addEventListener("message", (e: MessageEvent) => {
  const f = iframe;
  // Sandboxed without allow-same-origin: our frame always posts from the opaque origin "null".
  if (!f || e.source !== f.contentWindow || e.origin !== "null") return;
  const d = e.data as { channel?: unknown; type?: unknown } | null;
  if (d && d.channel === CHANNEL && d.type === "shell-ready") {
    // Exactly once per frame: a frame that navigated or reloaded itself stays empty.
    const html = pending;
    pending = null;
    if (html !== null) f.contentWindow?.postMessage({ channel: CHANNEL, type: "render", html }, "*");
    return;
  }
  if (d && d.channel === CHANNEL && d.type === "shell-refused") {
    pending = null;
    send({ kind: "refused", seq });
    return;
  }
  const msg = parseFrameMessage(e.data, nonce);
  if (!msg) return;
  if (msg.type === "size") {
    f.style.height = `${Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, window.innerHeight, msg.height))}px`;
  }
  send({ kind: "frame", seq, data: { channel: CHANNEL, nonce, ...msg } });
});

window.addEventListener("resize", () => {
  if (iframe) {
    const h = Number.parseInt(iframe.style.height, 10) || 0;
    iframe.style.height = `${Math.max(h, window.innerHeight)}px`;
  }
});

function boot(): void {
  const bg = new URLSearchParams(location.search).get("bg");
  document.documentElement.style.background = bg !== null && /^#[0-9a-fA-F]{6}$/.test(bg) ? bg : "transparent";
  document.body.style.margin = "0";
  // Focus can't leave a webview by Tab on its own: the app moves it back to the main window.
  for (const [id, dir] of [
    ["dn-start", "prev"],
    ["dn-end", "next"],
  ] as const) {
    document.getElementById(id)?.addEventListener("focus", () => send({ kind: "focus-out", dir }));
  }
}

Object.defineProperty(window, "__dnArtifactHost", {
  value: Object.freeze({
    render: (n: unknown, t: unknown) => {
      if (typeof n === "number" && Number.isSafeInteger(n) && typeof t === "string") void render(n, t);
    },
    focus: (t: unknown) => {
      if (typeof t === "string" && t === token) iframe?.focus();
    },
  }),
  configurable: false,
  writable: false,
});

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", boot);
else boot();
