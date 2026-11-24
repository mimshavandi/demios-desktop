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
- [ ] **WKWebView sub-frame IPC** (from the artifact review, W-artifacts): with an artifact open in its sandboxed frame, run `window.__TAURI_INTERNALS__?.invoke?.("list_conversations")` in the frame's Web Inspector console. It must be `undefined` or fail. wry's IPC handler must ignore sub-frames, and init scripts are main-frame only.
- [ ] WebRTC is not exposed to artifact frames (`typeof RTCPeerConnection` in the frame console is `"undefined"`, or connections fail).
- [ ] Links in answers don't navigate the window (the navigation guard logs "blocked navigation").
