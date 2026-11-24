// Runs first inside the sandboxed frame, before any artifact code. It reports size, runtime
// errors, console errors and CSP violations to the host with postMessage. It is NOT a security
// boundary: artifact code shares this realm and can forge or suppress any of these messages. The
// boundary is the iframe sandbox (opaque origin) plus CSP; see docs/security/artifact-sandbox.md.

import { CHANNEL, type FrameMessage } from "../protocol";
import type { SandboxIssue } from "../types";

(() => {
  const script = document.currentScript as HTMLScriptElement | null;
  const nonce = script?.dataset["nonce"] ?? "";
  const post = (msg: FrameMessage): void => {
    try {
      window.parent.postMessage({ channel: CHANNEL, nonce, ...msg }, "*");
    } catch {
      // Parent gone (frame being torn down).
    }
  };
  const issue = (i: SandboxIssue): void => post({ type: "issue", issue: i });
  const str = (v: unknown): string => {
    if (v instanceof Error) return `${v.name}: ${v.message}`;
    if (typeof v === "string") return v;
    try {
      return JSON.stringify(v) ?? String(v);
    } catch {
      return String(v);
    }
  };

  // WebRTC is not governed by CSP connect-src, so a peer connection with an attacker's STUN
  // server would be a network channel. Engines must disable it (docs/security/artifact-sandbox.md);
  // as defence in depth, remove it here before artifact code runs...
  for (const name of [
    "RTCPeerConnection",
    "webkitRTCPeerConnection",
    "RTCDataChannel",
    "RTCSessionDescription",
    "RTCIceCandidate",
  ]) {
    try {
      Object.defineProperty(window, name, { value: undefined, configurable: false, writable: false });
    } catch {
      // Not present in this engine.
    }
  }

  // ...and stop nested frames, which would be fresh realms with their own copy. CSP frame-src
  // blocks fetched frames but not `srcdoc` ones, so remove every nested frame before it loads
  // (frame loads are asynchronous; mutation records are delivered first). Shadow roots made with
  // attachShadow are watched too; declarative shadow roots are neutralised by the host
  // (document.ts) and in the parsing APIs patched below.
  // Artifact code runs later in this realm and may patch prototypes, so the guard only uses
  // built-ins captured here.
  const apply = Reflect.apply;
  const getter = (proto: object, key: string): ((this: unknown) => unknown) => {
    const d = Object.getOwnPropertyDescriptor(proto, key);
    if (!d?.get) throw new Error(`no getter ${key}`);
    return d.get as (this: unknown) => unknown;
  };
  const nodeType = getter(Node.prototype, "nodeType");
  const addedNodes = getter(MutationRecord.prototype, "addedNodes");
  const listLength = getter(NodeList.prototype, "length");
  const listItem = NodeList.prototype.item;
  const localName = getter(Element.prototype, "localName");
  const matches = Element.prototype.matches;
  const qsaElement = Element.prototype.querySelectorAll;
  const qsaFragment = DocumentFragment.prototype.querySelectorAll;
  const qsaDocument = Document.prototype.querySelectorAll;
  const removeElement = Element.prototype.remove;
  const observe = MutationObserver.prototype.observe;
  const attachShadow = Element.prototype.attachShadow;
  const replace = String.prototype.replace;
  const FRAMES = "iframe,frame,object,embed,portal,fencedframe";
  const ELEMENT = 1;
  const DOCUMENT = 9;
  const FRAGMENT = 11;

  const removeFrames = (root: Node): void => {
    const type = apply(nodeType, root, []) as number;
    const found: Element[] = [];
    if (type === ELEMENT && apply(matches, root, [FRAMES])) found[found.length] = root as Element;
    const qsa = type === ELEMENT ? qsaElement : type === FRAGMENT ? qsaFragment : type === DOCUMENT ? qsaDocument : null;
    if (qsa) {
      const list = apply(qsa, root, [FRAMES]) as NodeList;
      const n = apply(listLength, list, []) as number;
      for (let i = 0; i < n; i++) found[found.length] = apply(listItem, list, [i]) as Element;
    }
    for (let i = 0; i < found.length; i++) {
      const el = found[i] as Element;
      apply(removeElement, el, []);
      issue({ kind: "csp", message: `Blocked by the sandbox: nested <${String(apply(localName, el, []))}>`, detail: "Artifacts cannot embed frames." });
    }
  };
  const observer = new MutationObserver((records) => {
    for (let i = 0; i < records.length; i++) {
      const list = apply(addedNodes, records[i], []) as NodeList;
      const n = apply(listLength, list, []) as number;
      for (let j = 0; j < n; j++) removeFrames(apply(listItem, list, [j]) as Node);
    }
  });
  const watch = (root: Node): void => {
    apply(observe, observer, [root, { childList: true, subtree: true }]);
    removeFrames(root);
  };
  watch(document);
  Element.prototype.attachShadow = function (this: Element, init: ShadowRootInit): ShadowRoot {
    const root = apply(attachShadow, this, [init]) as ShadowRoot;
    watch(root);
    return root;
  };
  // Declarative shadow DOM (`<template shadowrootmode>`) would create closed roots that nothing
  // can observe; rename the attribute in every API that parses it.
  const DSD = /shadowrootmode/gi;
  const noDsd = (html: unknown): string => apply(replace, String(html), [DSD, "data-dn-blocked"]) as string;
  const docProto = Document.prototype as unknown as Record<string, (...a: unknown[]) => unknown>;
  for (const m of ["write", "writeln"]) {
    const orig = docProto[m];
    if (orig) docProto[m] = function (this: Document, ...a: unknown[]) { return apply(orig, this, a.map(noDsd)); };
  }
  for (const proto of [Element.prototype, ShadowRoot.prototype] as unknown as Record<string, (...a: unknown[]) => unknown>[]) {
    const orig = proto["setHTMLUnsafe"];
    if (orig) proto["setHTMLUnsafe"] = function (this: unknown, h: unknown, o?: unknown) { return apply(orig, this, [noDsd(h), o]); };
  }
  const docCtor = Document as unknown as Record<string, (...a: unknown[]) => unknown>;
  const parseUnsafe = docCtor["parseHTMLUnsafe"];
  if (parseUnsafe) docCtor["parseHTMLUnsafe"] = (h: unknown, o?: unknown) => apply(parseUnsafe, Document, [noDsd(h), o]);

  // Session history is shared with the host page: WebKit lets a sandboxed frame's
  // history.back()/go() move the app itself. Artifacts get no history or Navigation API
  // traversal. Locked (non-writable, non-configurable) so artifact code can't put them back.
  const blockHistory = (proto: object | undefined, names: string[], label: string): void => {
    if (!proto) return;
    for (const name of names) {
      try {
        Object.defineProperty(proto, name, {
          value: function blocked(): undefined {
            issue({ kind: "csp", message: `Blocked by the sandbox: ${label}.${name}()`, detail: "Artifacts cannot use session history." });
            return undefined;
          },
          writable: false,
          configurable: false,
        });
      } catch {
        // Already locked.
      }
    }
  };
  blockHistory(History.prototype, ["back", "forward", "go", "pushState", "replaceState"], "history");
  const nav = (window as unknown as { Navigation?: { prototype: object } }).Navigation;
  blockHistory(nav?.prototype, ["back", "forward", "traverseTo", "navigate", "reload"], "navigation");

  window.addEventListener("error", (e: ErrorEvent) => {
    const where = e.filename ? ` (line ${e.lineno}:${e.colno})` : "";
    // WebKit hides the details of errors in sandboxed frames ("Script error.", no error object).
    const muted = !e.error && /^script error\.?$/i.test(e.message);
    const message = muted
      ? "A script in this artifact threw an error (this engine hides the details for sandboxed content)"
      : e.message || str(e.error);
    issue({ kind: "error", message, detail: `${e.error?.stack ?? ""}${where}`.trim() });
  });
  window.addEventListener("unhandledrejection", (e: PromiseRejectionEvent) => {
    issue({ kind: "unhandledrejection", message: str(e.reason), detail: (e.reason as Error | undefined)?.stack ?? "" });
  });
  document.addEventListener("securitypolicyviolation", (e: SecurityPolicyViolationEvent) => {
    issue({
      kind: /^(https?|wss?):/i.test(e.blockedURI) ? "network" : "csp",
      message: `Blocked by the sandbox: ${e.effectiveDirective} ${e.blockedURI || "(inline)"}`,
      detail: "Artifacts run without network access. Inline the resource instead.",
    });
  });
  const origError = console.error.bind(console);
  console.error = (...args: unknown[]) => {
    issue({ kind: "console", message: args.map(str).join(" ").slice(0, 4000) });
    origError(...args);
  };

  let last = "";
  let scheduled = false;
  const sendSize = (): void => {
    scheduled = false;
    const el = document.documentElement;
    const body = document.body;
    const height = Math.ceil(Math.max(el.scrollHeight, body?.scrollHeight ?? 0));
    const width = Math.ceil(Math.max(el.scrollWidth, body?.scrollWidth ?? 0));
    const key = `${width}x${height}`;
    if (key === last) return;
    last = key;
    post({ type: "size", width, height });
  };
  const schedule = (): void => {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(sendSize);
  };
  new ResizeObserver(schedule).observe(document.documentElement);
  window.addEventListener("load", () => {
    if (document.body) new ResizeObserver(schedule).observe(document.body);
    schedule();
    post({ type: "ready" });
  });

  // Renderers (markdown, mermaid, react) report their own failures through this hook.
  (window as unknown as { __dnIssue: typeof issue }).__dnIssue = issue;
})();
