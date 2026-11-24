# Desktop e2e (T-SHELL-05)

Real-app tests: WebdriverIO drives the built app through
[tauri-driver](https://v2.tauri.app/develop/tests/webdriver/), which runs the platform's
WebDriver (WebKitWebDriver on Linux, msedgedriver on Windows).

- CI job **`e2e`** (required): `ubuntu-latest`, WebKitGTK.
- CI job **`e2e-windows`** (`continue-on-error` until it has been green three runs in a
  row): `windows-latest`, WebView2. Its e2e build turns on WebView2 remote debugging through
  the window config; see [Windows WebView2 session](#t-shell-06-windows-webview2-session).
  Its msedgedriver log is uploaded as the `msedgedriver-log` artifact.

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
**Symptom (fixed).** Every `POST /session` failed with `session not created:
DevToolsActivePort file doesn't exist` (60 s per try).

**Cause.** msedgedriver turns on WebView2 remote debugging only through the
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` environment variable (`--remote-debugging-port=0
--enable-automation …`). wry always sets `AdditionalBrowserArguments` through the WebView2
API (at least `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`), and on this
runtime the API value wins: the environment variable's switches never arrive, no DevTools
port opens, and no `DevToolsActivePort` file is written.

Evidence from CI:
- Launched by hand with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333`,
  nothing listens on 9333.
- `WEBVIEW2_USER_DATA_FOLDER` *is* honoured: the profile lands there, not in
  `%LOCALAPPDATA%\io.demios.app\EBWebView`. So it was never a user-data-folder mismatch.
- `ms:edgeOptions.args` (`tauri:options.args`) are passed to the app's command line, not to
  WebView2, so they can't carry the switches either.

**Fix.** The e2e-windows job builds the app with the debugging switches in the window's
`additionalBrowserArgs`. It generates `TAURI_CONFIG` from `tauri.conf.json` at build time:
each window gets its own value (or wry's default) plus `--remote-debugging-port=0
--enable-automation`. WebView2 then writes `DevToolsActivePort` into the profile folder
msedgedriver chose, and the session starts. Only the CI e2e build is affected; the app's
config and release builds are unchanged.

To reproduce locally on Windows, build the same way:
```powershell
$env:TAURI_CONFIG = '{"app":{"windows":[{"label":"main","title":"Demios","width":1280,"height":820,"minWidth":900,"minHeight":560,"additionalBrowserArgs":"--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port=0 --enable-automation"}]}}'
cargo build -p demios-desktop --features dev-fake-model,custom-protocol
```
(The CI step derives this from `tauri.conf.json`, so keep the two in step if you copy it.)

Done-check: `e2e-windows` green three runs in a row, then drop `continue-on-error` and make
it required.

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
