# @demios-desktop/artifact-sandbox

Sandboxed renderer for Claude-style artifacts (ADR-0004): `html`, `svg`, `mermaid`, `markdown`, `code` and single-file `react` (JSX/TSX), each in an opaque-origin iframe with no network, a `postMessage` bridge for size, runtime and console errors, and a version switcher with restore and "Fix this".

Strict TypeScript, framework-agnostic DOM (the desktop shell can wrap it in any UI framework). Security model and shell requirements: [`docs/security/artifact-sandbox.md`](../../docs/security/artifact-sandbox.md).

## Use

```ts
import { ArtifactView } from "@demios-desktop/artifact-sandbox";

const view = new ArtifactView(panelElement, {
  frameUrl: "artifact://localhost/frame/", // the frame shell, served on its own origin (see below)
  onRestore: (id, n) => view.setArtifact(rollback(id, n)), // dn-artifacts ArtifactStore::rollback
  onFix: ({ artifactId, version, issues }) => askModelToFix(artifactId, version, issues),
  allowNetwork: (id) => networkToggle.has(id), // ADR-0004 per-artifact toggle, default off
});
await view.setArtifact(artifact); // dn_core::artifact::Artifact as JSON
```

`ArtifactFrame` is the bare frame without the toolbar. Types in `src/types.ts` mirror `dn_core::artifact` (serde JSON).

## Embedding contract (desktop shell)

1. Serve `dist/frame/index.html` on a scheme/origin that is **not** the app's, e.g. `artifact://localhost/frame/`, with the `Content-Security-Policy` header from `dist/frame/csp.json` (`network` when the URL has `?net=1`, else `strict`) and `X-Content-Type-Options: nosniff`. Serve nothing else there.
2. Main webview CSP: `frame-src <that origin>`; no inline scripts needed by this package (styles use constructable stylesheets).
3. Ship `dist/index.js` and `dist/chunks/*` with the app. Renderer chunks load lazily: mermaid (≈5 MB) only when a diagram is shown.
4. Follow shell requirements S1–S6 in the threat model (IPC main-frame only, WebRTC disabled in the engine, …).

## React artifacts

One file that `export default`s a component. Transpiled **inside the frame** with sucrase (`jsx`, `typescript`, `imports`, automatic JSX runtime), then run as an inline script against vendored React. Imports allowed: `react`, `react-dom`, `react-dom/client`, `react/jsx-runtime`; anything else is a clear error shown under the artifact.

**Why sucrase, not esbuild-wasm:** pure JS (no WebAssembly, so the frame CSP needs no `'wasm-unsafe-eval'`), synchronous, a few hundred KB of minified JS instead of a ~10 MB `.wasm`, and it only strips types/JSX/imports, which is all a single-file artifact needs. esbuild-wasm would add bundling we don't use (there's nothing to fetch).

## Develop

```bash
npm ci
npm run build      # runtimes → dist/index.js, dist/chunks, dist/frame, dist/demo
npm run typecheck
npm test           # build + Playwright (Chromium and WebKit): flow + escape suites
npm run serve      # demo at http://127.0.0.1:4173 (artifact origin http://localhost:4175)
```

Tests need Playwright's Chromium and WebKit for `@playwright/test` 1.57.0 (`npx playwright install chromium webkit`).

## Known limitations

- **WebKit hides error details** of uncaught errors in sandboxed frames (`"Script error."`, no stack). The error bar then says a script failed without the message, so "Ask to fix" has less to go on on macOS/Linux. React artifacts are not affected (the renderer catches module errors itself, and the error boundary reports render errors). A possible fix is for the shell to serve the artifact document directly as the frame's response instead of writing it into the shell. This is unverified.
- Mermaid's ELK layout is not bundled (licence); diagrams use dagre.
- No optional libraries for React artifacts yet (recharts, lucide).
