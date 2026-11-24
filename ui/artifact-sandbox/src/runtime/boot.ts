// Boot script of the frame shell (dist/frame/index.html), served from the artifact origin with
// the artifact CSP as a response header. It announces itself, waits for exactly one "render" message from the
// parent window and replaces itself with the artifact document. After that it ignores messages.

import { CHANNEL } from "../protocol";

(() => {
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
