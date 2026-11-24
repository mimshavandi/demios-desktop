# Desktop e2e (T-SHELL-05, D-015)

Real-app tests: WebdriverIO drives the built app through
[tauri-driver](https://v2.tauri.app/develop/tests/webdriver/), which runs the platform's
WebDriver (WebKitWebDriver on Linux, msedgedriver on Windows). CI job `e2e` runs them on
`ubuntu-latest` and `windows-latest`.

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
