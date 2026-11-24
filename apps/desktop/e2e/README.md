# Desktop e2e (T-SHELL-05, D-015)

Real-app tests: WebdriverIO drives the built app through
[tauri-driver](https://v2.tauri.app/develop/tests/webdriver/), which runs the platform's
WebDriver (WebKitWebDriver on Linux, msedgedriver on Windows).

- CI job **`e2e`** (required): `ubuntu-latest`, WebKitGTK.
- CI job **`e2e-windows`** (`continue-on-error`): `windows-latest`, WebView2. It doesn't
  create a session yet; see [T-SHELL-06](#t-shell-06-windows-webview2-session) below. Its
  msedgedriver log is uploaded as the `msedgedriver-log` artifact.

## Scenario (`specs/chat.e2e.mjs`)
1. Launch, new chat, send. Assert the answer streams (`data-status="streaming"`), completes and quotes the prompt.
2. Send a second message and press **Ctrl+.** mid-answer. Assert it ends `cancelled` with "Stopped".
3. axe-core on the chat screen: no `serious` or `critical` violations.
4. Quit and relaunch on the **same data dir**. Assert one conversation, both user messages, and both answers with their final status.

## Scenario (`specs/artifacts.e2e.mjs`, T-ART-06)
1. "Make an HTML artifact" → the dev fake model calls `artifact_create`. Assert the answer has an artifact card (title, `v1`) and the pane renders v1 in the sandboxed frame. The version label reads `v1 · model` and the pane says "Made by …".
2. "Please update the artifact" → `artifact_update` (exact match). Assert the frame shows v2.
3. Go back to v1 and press **Restore this version**. Assert `v3 — restored from v1 · you` and v1's content.
4. **In-app escape cases** (threat model S6). Run as artifact code inside the frame, judged from outside it:
   - the frame is on `artifact://localhost/frame/` with `sandbox="allow-scripts"` and origin `null`;
   - no `__TAURI_INTERNALS__` or `ipc` in the frame;
   - no access to the parent's DOM or IPC;
   - `fetch` to the network, `ipc://` and `tauri://` is blocked, and a local spy server gets no request (fetch or image);
   - a raw `webkit.messageHandlers.ipc` message with a guessed key creates no conversation;
   - `RTCPeerConnection` is absent.

The app is a **debug** build with `--features dev-fake-model,custom-protocol`: the canned streaming model plus the UI embedded from `dist/`. `DEMIOS_DATA_DIR` points at a temp dir for the run, and `DEMIOS_FAKE_DELAY_MS=80` slows the stream so Stop lands mid-answer. Selectors are `data-testid` only.

## Run locally (Linux or Windows)
```sh
cargo install tauri-driver --version 2.0.6 --locked
npm --prefix apps/desktop ci && npm --prefix apps/desktop run build
cargo build -p demios-desktop --features dev-fake-model,custom-protocol
cd apps/desktop/e2e && npm ci && npm test        # Linux headless: xvfb-run -a npm test
```
- Linux needs `webkitgtk-webdriver` (Ubuntu 24.04+; `webkit2gtk-driver` on older releases).
- Windows needs an msedgedriver whose major version matches the installed WebView2 runtime; set `MSEDGEDRIVER=<path>`.

## T-SHELL-06: Windows WebView2 session
**Symptom.** Every `POST /session` fails with `session not created: DevToolsActivePort file
doesn't exist` (4 retries, 60 s each). Seen on `windows-latest` with WebView2 runtime
157.0.4341.46 and msedgedriver 157.0.4341.60 (runner copy, same major).

**Ruled out**
- *App crashes at start:* launched directly, it is still running after 8 s, with no output
  and no panic.
- *Driver/runtime mismatch:* same major version. The job also downloads the exact runtime
  version from Microsoft when the majors differ.
- *App pre-started:* the driver launches it. msedgedriver's log shows
  `Launching Microsoft Edge: "…\target\debug\demios-desktop.exe"` with `browserName:
  webview2` and `binary` set by tauri-driver.
- *Linux-specific setup:* the same suite, binary features and data-dir env pass on Linux.

**What the verbose msedgedriver log shows.** It sets
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` (including `--remote-debugging-port=0` and
`--enable-automation`, with no `--user-data-dir`), launches the binary, waits 60 s, then logs
`Failed to connect to msedge`. So the WebView2 browser process never reports a DevTools port
where the driver looks for it.

**Hypotheses, most likely first**
1. *User-data folder mismatch.* msedgedriver points WebView2 at its own temp profile
   (probably via `WEBVIEW2_USER_DATA_FOLDER`) and reads `DevToolsActivePort` from there.
   Tauri passes its own data directory (`…\io.demios.desktop\EBWebView`) through the
   WebView2 API, which may win, so the file is written elsewhere.
   Try: in e2e builds (`fake_model`), if `WEBVIEW2_USER_DATA_FOLDER` is set, create the
   main window in `setup` with `.data_directory(that)` (window config `create: false`).
   Or list `%LOCALAPPDATA%\io.demios.desktop\EBWebView` after a failed run to see
   whether a `DevToolsActivePort` file landed there.
2. *Browser-args precedence.* wry/Tauri pass their own `AdditionalBrowserArguments`
   (`--disable-features=msWebOOUI,…`). If the API value replaces the env var on this runtime,
   `--remote-debugging-port` never arrives. Try: window config `additionalBrowserArgs`
   that includes the env var's content, or set `browserArgs` from the env in e2e builds.
3. *Port vs pipe.* The log suggests `--remote-debugging-pipe`; not controllable through
   tauri-driver 2.0.6. Try tauri-driver 3.x when it is stable.
4. *Alternative driver.* `tauri-plugin-wdio-webdriver` (an embedded WebDriver, no
   msedgedriver) would avoid the handshake. It is a new dependency, so it needs the lead's
   OK.

Done-check for T-SHELL-06: `e2e-windows` green three runs in a row, then drop
`continue-on-error` and make it required.

## macOS: manual release checklist
macOS has no WebDriver for WKWebView, so run this before every release, on a signed build where possible.

- [ ] `npm run dev:app` (or the release build plus a provider key): the app launches; the title bar shows traffic lights over the overlay title bar, and the window can be dragged by the title bar.
- [ ] New chat → send → the answer streams; markdown and code highlighting render; Copy on a code block works.
- [ ] Second send → **⌘.** stops it → "Stopped" with Retry; Retry streams a new answer.
- [ ] Quit (⌘Q) → relaunch → the conversation and all messages are listed; a stopped answer keeps its partial text.
- [ ] Delete a chat → Undo restores it; delete again, quit within 10 s, relaunch → it stays deleted.
- [ ] Settings → Models: save a provider key → macOS asks for keychain access the first time; "Key saved". Remove key → Undo restores it.
- [ ] ⌘N, ⌘,, ⌘\, ⌘⇧C work; Tab reaches every control with a visible focus ring; VoiceOver announces "Answer complete" once per answer.
- [ ] Light and dark appearance (System Settings) both follow the design tokens.
- [ ] **Artifacts:** "Make an HTML artifact" opens the pane with v1. "Please update the artifact" shows v2. Restoring v1 makes v3. Light and dark both read correctly.
- [ ] **WKWebView sub-frame IPC** (artifact review, S1): open an artifact, choose the artifact frame in Web Inspector, and run the checks below in its console.
  - `typeof window.__TAURI_INTERNALS__` is `"undefined"`.
  - `window.webkit.messageHandlers.ipc.postMessage(JSON.stringify({cmd:"create_conversation",callback:1,error:2,payload:{},options:{},__TAURI_INVOKE_KEY__:"guess"}))` creates **no** chat. The app log shows `__TAURI_INVOKE_KEY__ expected … but received guess`.
  - wry's WKWebView handler does **not** filter sub-frames, and it reports the frame's URL (`artifact://localhost/frame/`), which Tauri counts as a local origin. The invoke key, injected only into the main frame, is what stops this.
- [ ] WebRTC is not exposed to artifact frames: in the frame console, `typeof RTCPeerConnection` is `"undefined"`. WKWebView can't turn WebRTC off without a private API, so the in-frame bridge is the control there.
- [ ] Network stays off: in the frame console, `fetch("https://example.com")` is blocked by CSP. **Allow network for this artifact** asks a second time before it turns on.

## Windows: manual artifact checks (until `e2e-windows` runs)
WebView2 runs the app's init scripts in **sub-frames too**. Tauri's IPC object, which holds the invoke key, therefore starts out inside the artifact frame. The frame shell deletes it before artifact code runs, or refuses to render if it can't (`ui/artifact-sandbox/src/runtime/boot.ts`).
- [ ] In DevTools, choose the artifact frame (`http://artifact.localhost/frame/`): `typeof window.__TAURI_INTERNALS__` and `typeof window.chrome?.webview` are `"undefined"`.
- [ ] `edge://webrtc-internals` shows no peer connection after `new RTCPeerConnection()` in the frame. The window runs with `--force-webrtc-ip-handling-policy=disable_non_proxied_udp` (`tauri.windows.conf.json`).
- [ ] Links in answers don't navigate the window (the navigation guard logs "blocked navigation").
