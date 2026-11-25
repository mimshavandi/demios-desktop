# ADR-0012: Artifacts on Windows render in a separate, capability-less webview

**Status:** Accepted (2026-11-25) · **Date:** 2026-11-25 · **Deciders:** project owner, lead · **Extends:** [ADR-0004](0004-artifacts-runtime.md), [ADR-0002](0002-desktop-shell.md) · **Issue:** #75 (threat model R8)

## Context
On macOS and Linux an artifact renders in a sandboxed iframe inside the main webview, on its own origin, with an opaque origin and a strict CSP. Tauri's IPC scripts, which hold the invoke key, run in the main frame only, so the iframe can't call the app.

**WebView2 doesn't work that way.** wry adds every init script with `AddScriptToExecuteOnDocumentCreated` and ignores "main frame only", so Tauri's IPC object is defined in every frame.

- The object is non-configurable, so the frame shell can't delete it.
- Tauri treats every custom scheme as a local origin.

An artifact iframe in the main webview could therefore call every command the main webview may call. Until now artifacts were off on Windows (the pane said so).

Upstream fixes are open, wry #1874 among them. The wry maintainers lean towards documenting the behaviour in v2, so we can't plan on a fix.

The #86 spike tested a child webview of the main window, labelled `artifact`. The app capability is scoped to `webviews: ["main"]` (#85), and no capability names `artifact`, so Tauri's ACL refuses every call from it, although the IPC object is there. All five checks passed on Windows, Linux and macOS:
- (a) commands are refused;
- (b) no channel data;
- (c) no events;
- (d) the main webview's IPC is unaffected;
- (e) no access to the main webview's DOM or storage.

The owner accepted Tauri's `unstable` feature (multiwebview) for this.

## Decision
1. **On Windows**, the artifact frame lives in a **child webview of the main window**, labelled `artifact` (`apps/desktop/src/artifact_child.rs`).
   - No capability may name it: the test `no_capability_names_the_artifact_webview` enforces this.
   - Its isolation is the ACL.
   - Inside it, a small host page (`ui/artifact-sandbox/src/host/host.ts`) holds the **same** sandboxed iframe, CSP and one-render-per-frame protocol as on the other platforms. So the iframe sandbox stays as a second layer.
2. **macOS and Linux keep the iframe in the main webview.** There, init scripts really are main-frame only, and on Linux a child webview has a GTK layout bug (below).
3. **The child copies the main window's WebView2 browser arguments** from the one merged config (`main_browser_args`).
   - WebView2 webviews sharing a data folder must use identical arguments, or the later one silently never loads.
   - A unit test fails if the two diverge.
   - We rejected a separate data folder because it would put the child's storage outside "Delete all local data".
4. **Messages.** Everything between the child and the app goes through Rust; the child calls no Tauri command.
   - Main UI → `artifact_child_render`. Rust stores the document, then tells the host page to fetch it once (`GET /host/doc`).
   - Host page → `POST /host/event` → Rust → the main UI's channel.
   - Both routes need a per-session token. The token reaches the host page through webview eval and never appears in a URL.
   - Both routes are served only to the `artifact` webview.
   - Frame messages keep their nonce. They are validated in the host page and again in the main UI.
5. **Layout and overlays.**
   - The main UI keeps a placeholder where the frame goes and syncs the child's bounds to it on every layout change: resize, scroll, sidebar toggle, pane open or close.
   - The child is drawn above the whole main page, so it is **hidden while any app overlay is over it**: dialog, menu, select list, toast or tooltip.
   - One central signal (`ui/src/overlay.ts`) watches the portalled Radix overlays. No component reports itself.
6. **Focus and keys.**
   - The placeholder passes focus into the child. Tab out of the child comes back as an event, and focus moves on in the main window.
   - Escape and the app's own shortcuts pressed in the frame are forwarded. Escape closes the pane; only an allowlist of shortcuts runs. Artifact code can fake these reports, so none of the allowed shortcuts deletes or sends anything.
   - The pane's labels, issue bar and live announcements stay in the main webview.
7. **Tauri is pinned to the 2.11 minor** (`~2.11`; the npm `@tauri-apps` packages likewise). `unstable` has no semver promise.
   - The isolation spec (`apps/desktop/e2e/specs/artifact-child.e2e.mjs`) is part of the required `e2e-windows` job.
   - **Every Tauri upgrade must pass it.** CI and `e2e/README.md` say so.

## Options considered

| Option | Isolation | Effort | UX | New dependency |
|---|---|---|---|---|
| **Child webview, capability-less (chosen)** | Tauri ACL, plus the iframe sandbox | Medium: bounds sync, overlays, focus | Same side pane | None (Tauri feature `unstable`) |
| Wait for upstream (wry #1874) | Main-frame-only scripts | None | Artifacts off on Windows meanwhile | None |
| Separate artifact window | Tauri ACL | Low | A second window, worse | None |
| Raw wry webview without Tauri scripts | No IPC object at all | High: protocol, lifetime, DPI, own data folder | Same side pane | wry as a direct dependency |

## Trade-offs
- **`unstable` changes every window's own webview** into an auto-resized child, on all platforms.
  - e2e checks that the main webview follows window resizes on Windows, Linux and macOS (`specs/window.e2e.mjs`).
  - Dragging the window and the macOS traffic lights are a manual release check.
- **No semver promise**, hence the pin and the upgrade gate.
- **Linux:** with a child webview added, GTK stacks it below the main webview, and the main webview takes only half the height. The bug doesn't occur without a child. Linux never creates one, so it is noted here and not fixed.
- **The child is a native surface.** It can't be clipped, blended or put under page content.
  - It is hidden rather than shown as a snapshot while an overlay is open: the pane shows an empty frame area for that time.
  - Page zoom other than 100% would misplace it (the app has no zoom control).
- **Two accessibility trees.** The frame area is a labelled region in the main window; the artifact's own content is in the child's tree, as it was in the iframe's.

## Consequences
- Artifacts work on Windows: the "not available on Windows" message is gone.
- Threat model R8 changes from "off on Windows" to "isolated by the ACL in a separate webview" ([docs/security/artifact-sandbox.md](../security/artifact-sandbox.md)).
- If wry starts honouring main-frame-only scripts on WebView2, Windows can move back to the iframe-in-main path, and this ADR is superseded.
- Never grant a capability to the `artifact` webview, and never add a `windows` or `*` entry to a capability.

## What users see (Windows)
- Artifacts open in the right-hand pane, as on macOS and Linux. The "Artifacts aren't available on Windows yet" message is gone.
- While a dialog, menu, toast or tooltip is over the pane, the artifact area is blank. It comes back when the overlay closes.
- Esc inside an artifact closes the pane. Ctrl+N, Ctrl+, and the other app shortcuts work from inside an artifact. Tab moves from the artifact back into the app.
- Artifacts are AI-generated and labelled as such, as on the other platforms.

## Action items
- #75: build it; this ADR and the PR that closes #75.
- On every Tauri upgrade: run `e2e-windows`, plus the manual window checks in `apps/desktop/e2e/README.md`.
- Follow wry #1874; supersede this ADR if it lands.
