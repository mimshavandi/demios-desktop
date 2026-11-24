// Boot script of the frame shell (dist/frame/index.html), served from the artifact origin with
// the artifact CSP as a response header. It announces itself, waits for exactly one "render" message from the
// parent window and replaces itself with the artifact document. After that it ignores messages.

import { CHANNEL } from "../protocol";

/**
 * Host objects a desktop webview may have injected into this frame. WebView2 runs the app's
 * init scripts in sub-frames too (wry: "scripts are always added to subframes" on Windows), so
 * Tauri's IPC object, which holds the invoke key, can exist here. Removed before any artifact
 * code runs; if one can't be removed, the shell refuses to render (fail closed).
 */
const HOST_GLOBALS = ["__TAURI_INTERNALS__", "__TAURI__", "__TAURI_EVENT_PLUGIN_INTERNALS__", "__TAURI_METADATA__", "__TAURI_IPC__", "__TAURI_POST_MESSAGE__", "ipc"] as const;

function stripHostGlobals(): boolean {
  const w = window as unknown as Record<string, unknown>;
  for (const k of HOST_GLOBALS) {
    if (!(k in w)) continue;
    try {
      delete w[k];
    } catch {
      /* non-configurable: checked below */
    }
  }
  const chrome = w["chrome"] as Record<string, unknown> | undefined;
  if (chrome && typeof chrome === "object" && "webview" in chrome) {
    try {
      delete chrome["webview"];
    } catch {
      /* checked below */
    }
  }
  const left = HOST_GLOBALS.filter((k) => k in w && w[k] !== undefined);
  const webview = chrome && typeof chrome === "object" && "webview" in chrome && chrome["webview"] !== undefined;
  return left.length === 0 && !webview;
}

(() => {
  // Only ever a child of the host: as a top-level page (a link or a stray navigation) it has
  // nobody to render for, so it stays inert.
  if (window.top === window) {
    stripHostGlobals();
    document.documentElement.textContent = "Artifacts open in the Demios artifact pane.";
    return;
  }
  if (!stripHostGlobals()) {
    // Never hand artifact code a realm that still reaches the app's IPC.
    document.documentElement.textContent = "This artifact can't be shown safely in this window.";
    window.parent.postMessage({ channel: CHANNEL, type: "shell-refused" }, "*");
    return;
  }
  let done = false;
  const onMessage = (e: MessageEvent): void => {
    if (done || e.source !== window.parent) return;
    const d = e.data as { channel?: unknown; type?: unknown; html?: unknown } | null;
    if (!d || d.channel !== CHANNEL || d.type !== "render" || typeof d.html !== "string") return;
    done = true;
    window.removeEventListener("message", onMessage);
    // Same document, same CSP: document.open keeps the policy and clears all listeners.
    document.open();
    document.write(d.html);
    document.close();
  };
  window.addEventListener("message", onMessage);
  window.parent.postMessage({ channel: CHANNEL, type: "shell-ready" }, "*");
})();
