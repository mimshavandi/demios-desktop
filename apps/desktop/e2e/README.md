# Desktop e2e (T-SHELL-05)

Real-app tests: WebdriverIO drives the built app through
[tauri-driver](https://v2.tauri.app/develop/tests/webdriver/), which runs the platform's
WebDriver (WebKitWebDriver on Linux, msedgedriver on Windows).

- CI job **`e2e`** (required): `ubuntu-latest`, WebKitGTK.
- CI job **`e2e-windows`** (required): `windows-latest`, WebView2. Its e2e build turns on WebView2 remote debugging through
  the window config; see [Windows WebView2 session](#t-shell-06-windows-webview2-session).
  Its msedgedriver log is uploaded as the `msedgedriver-log` artifact.

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

Done: `e2e-windows` was green three runs in a row on the fix, and no longer has
`continue-on-error`.

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
  - `window.webkit.messageHandlers.ipc.postMessage(JSON.stringify({cmd:"create_conversation",callback:1,error:2,payload:{},options:{},__TAURI_INVOKE_KEY__:"guess"}))` creates **no** chat (check the chat list). Don't copy app logs into issues: Tauri's rejection message includes the live invoke key.
  - wry's WKWebView handler does **not** filter sub-frames, and it reports the frame's URL (`artifact://localhost/frame/`), which Tauri counts as a local origin. The invoke key, injected only into the main frame, is what stops this.
- [ ] WebRTC is not exposed to artifact frames: in the frame console, `typeof RTCPeerConnection` is `"undefined"`. WKWebView can't turn WebRTC off without a private API, so the in-frame bridge is the control there.
- [ ] Network stays off: in the frame console, `fetch("https://example.com")` is blocked by CSP. **Allow network for this artifact** asks a second time before it turns on.
- [ ] Links in answers don't navigate the window (the navigation guard logs "blocked navigation").

## Preview builds (unsigned)
For testing real models before releases are signed (#51). The `preview` workflow (`.github/workflows/preview.yml`) makes a release build for Apple silicon: `tauri build`, with local inference (llama.cpp + Metal) on, no dev fake model and no e2e flags. A step checks that the binary contains no dev-only strings. The build is **not signed or notarized** and **not published**: it's a workflow artifact, kept 14 days and visible to repo collaborators only.

- **Run it:** Actions › preview › Run workflow (on any branch), or push a tag `preview-*`. From a terminal: `gh workflow run preview --ref <branch>`.
- **Get it:** the run's Artifacts list has `Demios-Desktop-<version>-preview-<sha>-macos-arm64` (a `.dmg` and an `.app.zip`), or use `gh run download <run-id>`. The run summary lists the commit, version and sizes. Settings › General › About shows "Preview build (unsigned), commit <sha>".
- **Open it:** macOS blocks unsigned apps the first time. Right-click (Control-click) the app › **Open** › **Open**. On macOS 15 and later, if there is no Open button, try to open the app once, then use **System Settings › Privacy & Security › Open Anyway**. Or run `xattr -dr com.apple.quarantine "/Applications/Demios Desktop.app"`.
- **Expected:** `codesign -dv` reports an ad-hoc signature (or none), and `spctl -a -vv` reports *rejected*. Keychain prompts name the app as unsigned, and you'll be asked again after each new build.

Real-model checklist (#51), on the preview build:
- [ ] Models › download a small model from the built-in list. Pause and resume three times: it resumes from where it stopped and ends **Ready** (checksum verified).
- [ ] Start a download, quit (⌘Q) mid-download, relaunch: the row is **Paused** at its size, and Resume finishes it.
- [ ] Chat with it: the answer streams and the model badge says **On this device**.
- [ ] Switch models while a reply is streaming: the reply finishes on the old model, and memory (Activity Monitor) drops once it ends, with no second copy of the weights.
- [ ] With nothing streaming, switch models five times: memory stays at one model's footprint.
- [ ] Two chats on one local model: sending in the second while the first streams says the model is **busy**, without a crash or a mixed-up answer.
- [ ] Hugging Face: Add from Hugging Face › search a GGUF repo › pull a quant. It downloads, verifies and chats. A gated repo asks for access or a token. A `demios://models/add` link opens the add screen.

## demios:// links: manual checks per OS (T-INF-12)

These need an installed build, so CI can't run them. On every OS, open these from a browser or terminal:
- a valid link: `demios://models/add?repo=Qwen/Qwen2.5-0.5B-Instruct-GGUF&quant=Q4_K_M`;
- an invalid link: `demios://models/add?repo=x&download=1`.

Expect:
- The valid link opens Settings › Models on the confirmation, with the Q4_K_M version first and "Chosen by the link". **Nothing downloads until you click Download.**
- The invalid link is ignored; the app logs "ignored a demios:// link".
- A second valid link while the confirmation is open is ignored. After "Not now", a new link opens again.
- A composer draft you were typing is still there when you go back to the chat.

**macOS (.app / DMG)**
- `Contents/Info.plist` › `CFBundleURLTypes` lists **only** the `demios` scheme.
- With the app running, a link brings the same window forward. No second window or Dock icon appears.
- Cold start: quit the app, then open a link. The app starts and shows the confirmation, not the last chat.

**Windows (MSI and NSIS)**
- The installer registers **only** `HKCU\Software\Classes\demios`, with no other keys or file associations. Check with `reg query HKCU\Software\Classes\demios /s`.
- Uninstalling removes that key.
- With the app running, a link focuses it, and no second process stays in Task Manager.
- Cold start shows the confirmation.

**Linux (.deb / .rpm / AppImage / Flatpak)**
- The `.desktop` file's `MimeType=` is **only** `x-scheme-handler/demios;`. `xdg-mime query default x-scheme-handler/demios` names Demios.
- A packaged release doesn't rewrite `~/.local/share/applications` at launch. Only dev builds and the AppImage register the scheme at run time.
- Single instance works over D-Bus: a link while the app runs focuses the existing window, and `pgrep -c demios-desktop` stays 1.
- Flatpak: a link from the host browser reaches the sandboxed app through the portal and opens the confirmation.

## Windows: artifacts are off (threat model R8)
WebView2 runs the app's init scripts in sub-frames too, and Tauri defines its IPC object there so that it can't be removed. The artifact frame shell refuses to run artifact code in a frame that can still reach the app's IPC. So for now artifacts are saved but not shown on Windows, and the pane says "Artifacts aren't available on Windows yet". On Windows the `artifact` scheme serves an inert page with no script. `e2e-windows` asserts the gate message and that no artifact frame is created. Making artifacts safe on WebView2 is tracked in #75.

## Release checklist: artifacts
Run these on the signed release build of each platform, in addition to the lists above.

**macOS (WKWebView)**
- [ ] Create, update and restore an artifact (HTML, SVG, Markdown, Mermaid, React, code): each renders, and the version label and "AI-generated · Made by <model>" are right.
- [ ] In the artifact frame's Web Inspector console:
  - `typeof __TAURI_INTERNALS__`, `typeof ipc` and `typeof RTCPeerConnection` are `"undefined"`;
  - `fetch("https://example.com")` is blocked by CSP;
  - `parent.document` throws.
- [ ] The raw `webkit.messageHandlers.ipc` message above creates no chat.
- [ ] **Ask to fix** sends a message whose report is inside the `ARTIFACT_REPORT` block.
- [ ] **Allow network for this artifact** needs the second confirm. After the model makes a new version, network is off again.
- [ ] Delete all local data → the confirm lists the artifact count → afterwards no artifact is listed.

**Windows (WebView2)**
- [ ] An artifact request saves the artifact: the card shows. **Open** shows "Artifacts aren't available on Windows yet" and creates no frame.
- [ ] `http://artifact.localhost/frame/` opened in DevTools serves the inert page (no script).

**Linux (WebKitGTK, release build)**
- [ ] The same artifact flow as macOS, on the release `.deb`/AppImage, not just the debug e2e build.
- [ ] The artifact frame loads, so the navigation guard allows `artifact://localhost/frame/` in a release build too.
- [ ] `typeof RTCPeerConnection` is `"undefined"` in the main window too (WebKitGTK `enable-webrtc` is off).
