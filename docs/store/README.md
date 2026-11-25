# Store readiness

> **Store submissions are deferred.** The first releases ship as installers on GitHub Releases and demios.io: `.dmg` (macOS), `.exe` (Windows), and AppImage, `.deb` and `.rpm` (Linux). Microsoft Store, Flathub and Mac App Store submissions are on hold. The MSIX and Flatpak packaging work stays in the repo, but nothing is submitted. The signing, privacy, accessibility and updater work below still applies to the direct installers.

What it takes to ship Demios Desktop to its distribution targets: macOS direct download (Developer ID + notarization), Microsoft Store (MSIX), signed direct Windows download as a fallback, and Flathub. The Mac App Store is out of scope for now. No developer accounts or certificates exist yet, so nothing here signs or submits anything, and no secrets, certificates or account ids go in the repo.

| File | What it is |
|---|---|
| [readiness-audit.md](readiness-audit.md) | T-STORE-01 audit: ranked findings with evidence, a verdict per target, a Mac App Store note and the owner items |
| [updates.md](updates.md) | T-STORE-08: signed updates for direct downloads, the signing-key setup and what a release must publish |
| [review-notes.md](review-notes.md) | Draft reviewer and certification notes (Microsoft Store, Flathub PR) |
| [`packaging/msix/`](../../packaging/msix/README.md) | T-STORE-04: MSIX manifest template, build script, CI test package and the owner's Partner Center steps |
| [`apps/desktop/.app-review-readiness.json`](../../apps/desktop/.app-review-readiness.json) | Manual-check record in the app-store-compliance skill's format. It sits in the app folder because the skill's guard reads it from the folder it scans. `desktop_checks` adds the desktop gates the guard does not know about |

**Status (2026-11-24, before store submissions were deferred):** every target is blocked. macOS direct and Windows direct are close: they mostly need signing plus a privacy policy. The Microsoft Store and Flathub need packaging, listing work and the features in the listing actually present in the shell. See the verdict table in the audit.

## Proposed fix PRs (one concern each)

| Task | Concern | Findings | Needs first |
|---|---|---|---|
| T-STORE-02 | macOS: `bundle.macOS` config (hardened runtime, empty entitlements, `minimumSystemVersion`, Info.plist usage strings) and a manual-dispatch sign → notarize → staple job using CI secrets only | S-01, S-15, S-16 | Developer ID (to run it; the config can land before) |
| T-STORE-03 | Windows Authenticode signing hook (`bundle.windows.signCommand`, timestamp) for MSI/NSIS | S-02 | Certificate or cloud signing (to run it) |
| T-STORE-04 | MSIX packaging: manifest template, capabilities (`runFullTrust`, `internetClient`), identity filled in at build time, tile assets, clean-VM test | S-03, S-17, S-24 (assets) | Partner Center name reservation (to run it) |
| T-STORE-05 | Privacy policy draft (data-collection declaration, network flows, data paths per OS) and a link in Settings › About | S-04, S-27 (docs) | Owner approves the text and the hosting URL |
| T-STORE-06 | Flatpak manifest, AppStream metainfo (OARS, releases, screenshots), `.desktop` file, minimal `finish-args`. In [packaging/flatpak/](../../packaging/flatpak/README.md); submission is the owner's | S-05, S-06, S-13, S-14, S-20 (OARS) | App id `io.demios.app` (decided); owner submits |
| T-STORE-07 | Third-party licence notices generated at build time, plus an "Open-source licences" view | S-09 | Lead approves the notice tool |
| T-STORE-08 | Signed Tauri updater behind an `updater` feature, direct builds only, private key in CI secrets | S-10 | Lead approves the dependency; owner picks the update host |
| T-STORE-09 | AI disclosure line, per-message provenance, machine-readable "AI-generated" marking on exports and artifacts, `docs/ai-literacy.md` | S-11 | Owner/legal view on provider vs deployer |
| T-STORE-10 | Flatpak MCP mode (HTTP-only / bundled servers, with an in-app explanation) | S-12 | Owner picks option A/B/C |
| T-STORE-11 | Honest privacy UI: remove the inert telemetry switch, explain the BYOK data flow per provider, "Delete all local data" | S-18, S-19, S-27 | – |
| T-STORE-12 | Bundle metadata (`publisher`, `copyright`, `category`, descriptions, `homepage`), draft listing copy for the shipped build, release-build screenshots | S-07, S-23, S-24 | Owner supplies publisher and URLs and approves the copy |
| T-STORE-13 | Accessibility statement (`docs/accessibility.md`) after a screen-reader pass | S-26 | – |
| T-STORE-14 | "Report a problem" and "Report this answer" feedback path | S-21 | Owner supplies the support contact |

**Not T-STORE work:** S-28 (absolute MCP command paths or login-shell PATH on macOS) is a W-tools follow-up. Wiring local models and artifacts into the shell (S-07, S-08) belongs to the T-INF and T-ART workers. Store submission waits for it, or the listing is trimmed to match.
