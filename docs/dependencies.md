# Dependencies

Every direct dependency is listed here with why and its licence (ADR-0009). Anything not already listed here needs a maintainer's OK before the PR.

| Crate | Used by | Why | Licence | Added in |
|---|---|---|---|---|
| serde, serde_json | all | (de)serialisation of contracts | MIT/Apache-2.0 | T-CORE-01 |
| thiserror | all | typed errors | MIT/Apache-2.0 | T-CORE-01 |
| uuid (v7) | dn-core | time-ordered, sync-safe ids | MIT/Apache-2.0 | T-CORE-01 |
| futures-core | dn-core | `Stream` trait for `ChatModel::chat` | MIT/Apache-2.0 | T-CORE-01 |
| keyring 3 | dn-secrets | OS keychain for BYOK keys and MCP tokens (ADR-0007). Features `apple-native`, `windows-native`, `async-secret-service` + `tokio` + `crypto-rust`, so there are no C deps (libdbus/openssl) | MIT/Apache-2.0 | T-PRV-03 |
| tokio | dn-agent, dn-secrets, dn-providers | async runtime; `sync` for streams/cancellation, `rt` for `AsyncSecrets::spawn_blocking`; dn-providers also `time` for SSE first-byte/idle timeouts (T-PRV-04); approved wave-1 dependency | MIT | T-AGT-02 |
| reqwest 0.13 | dn-providers | HTTPS client for BYOK providers. `rustls` (aws-lc-rs, platform verifier) + `http2` + `charset`; no OpenSSL. SSE parsing is hand-rolled (`dn_providers::sse`) | MIT/Apache-2.0 | T-PRV-01 |
| zeroize 1 | dn-secrets | wipe `Secret` buffers on drop (`Zeroizing<String>`); already a transitive dep of keyring | MIT/Apache-2.0 | T-PRV-03 |
| aws-lc-rs 1.19 (no default features; `aws-lc-sys`) | dn-sync | device identity and pairing (ADR-0014 §2): Ed25519 signatures, X25519 keys, HMAC-SHA256 pairing proof, SHA-256, system RNG. Already built into the app as rustls's crypto provider, same version, so no new code is compiled; direct use needs the lead's approval (asked in the PR) | ISC AND (Apache-2.0 OR ISC) | #233 |
| ts-rs 12 (optional, `ts` feature; `serde-compat`, no default features) | dn-core | TypeScript bindings for the UI in `crates/dn-core/bindings/`; not in the default build | MIT | T-CORE-04 |
| serde_yaml_ng 0.10 | dn-agent | YAML manifests (`agent.yaml`, `skill.yaml`, prompt `manifest.yaml`). Maintained fork of the archived `serde_yaml` (chosen as the maintained successor) | MIT | T-AGT-01 |
| jsonschema 0.63 (`default-features = false`) | dn-agent | validate manifests against bundled Draft 2020-12 schemas; no remote/file `$ref` resolution (approved by lead) | MIT | T-AGT-01 |
| rusqlite 0.43 (`bundled`) | dn-store, dn-tools, dn-artifacts, dn-knowledge | SQLite store and migration runner (ADR-0007); bundled SQLite so all three OSes build the same. dn-tools: side-effect ledger (`tool_ledger_*`). dn-artifacts: artifact store (`artifact_*`, ADR-0004). dn-knowledge: knowledge index (`knowledge_*`, FTS5 from the bundled SQLite, #116) | MIT (SQLite: public domain) | T-CORE-02 |
| rmcp 3.11 (client, transport-async-rw, streamable-http-client-reqwest, auth) | dn-tools | MCP client over stdio + Streamable HTTP; OAuth (PKCE, discovery, DCR) via its `auth` feature (pulls `oauth2`, `url`). Dev: `server`, `transport-io`, `transport-streamable-http-server` for test servers | Apache-2.0 | T-CON-01 |
| tokio | dn-tools | async runtime, child processes, timeouts, approval channels | MIT | T-CON-01 |
| reqwest 0.13 (rustls) | dn-tools | HTTP client handed to rmcp (no redirects for MCP traffic) | MIT/Apache-2.0 | T-CON-01 |
| axum 0.8 (dev only) | dn-tools tests | in-process Streamable HTTP + OAuth test servers | MIT | T-CON-01 |
| process-wrap 10 (tokio1, process-group, job-object) | dn-tools | stdio MCP servers run in their own process group (Unix) / job object (Windows) so shutdown also stops grandchildren (e.g. npx → node); same major version rmcp already uses | Apache-2.0 OR MIT | T-CON-01 |
| tokio | dn-inference, dn-models, dn | async runtime, channels, fs for downloads | MIT | T-INF-01 |
| llama-cpp-2 (+ llama-cpp-sys-2, builds llama.cpp) | dn-inference | in-process llama.cpp backend; `metal` on macOS, `cuda`/`vulkan` features | MIT/Apache-2.0 (llama.cpp: MIT) | T-INF-02 |
| minijinja 3.1 (`builtins`, `serde`, `json`, `fuel`, `macros`; no default features), minijinja-contrib 3.1 (`pycompat`) | dn-inference (`llamacpp`) | renders a GGUF's own Jinja chat template (llama-cpp-2 doesn't expose llama.cpp's). Templates are untrusted: no loader (no includes or file access), a fuel budget, a recursion limit and an output cap | Apache-2.0 | T-INF-23 |
| reqwest 0.13 (rustls) | dn-models, dn-inference | resumable model downloads (HTTPS only, read timeout); `/v1/embeddings` for OpenAI-compatible servers (chat goes through dn-providers) | MIT/Apache-2.0 | T-INF-05, T-INF-06 |
| sha2 | dn-models, demios-desktop, dn-knowledge | SHA-256 verification of downloads; fingerprints of MCP tools; knowledge index: skip unchanged files | MIT/Apache-2.0 | T-INF-05 |
| sysinfo (`system`; `disk` in dn-models) | dn-models, dn | RAM detection for the fit estimator; free disk space before a model download; peak RSS in `dn bench` | MIT | T-INF-03 |
| clap (derive) | dn | `dn` command-line parsing | MIT/Apache-2.0 | T-INF-04 |
| tauri 2, tauri-build 2 | apps/desktop | desktop shell, typed IPC, capabilities (ADR-0002). Pinned to `tauri ~2.11` / `tauri-build ~2.6` with the `unstable` feature (multiwebview) for the Windows artifact webview (ADR-0012, owner approved); every upgrade must pass `e2e-windows` | MIT/Apache-2.0 | T-SHELL-01, #75 |
| tokio (`sync`, `macros`, `time`, `rt`) | apps/desktop | cancel a streaming answer (`select!` + `oneshot`); timer for the dev fake model | MIT | T-SHELL-01 |
| tauri-plugin-opener 2 | apps/desktop | open the "Report a problem" form in the system browser; called from Rust only, its JS commands are not granted (lead approved 2026-11-24); links in answers also open through it, from the `open_link` command after a native confirm (T-SHELL-09) | MIT/Apache-2.0 | T-STORE-14 |
| tauri-plugin-dialog 2 | apps/desktop | native confirms shown from Rust only: "start this program?" before a local MCP server is launched, "open this link?" with the full URL for links in answers (T-SHELL-09), and the file picker for **Add files** in a project (T-PRJ-01); its JS commands are not granted (lead approved) | MIT/Apache-2.0 | T-SHELL-08 |
| url 2 | apps/desktop | parse the MCP OAuth sign-in URL (scheme, host, no userinfo) before opening it; already in the tree via reqwest (lead approved) | MIT/Apache-2.0 | T-SHELL-08 |
| tauri-plugin-deep-link 2 | apps/desktop | the OS hands `demios://models/add` links to the app; handled in Rust only (`AddModelLink::parse` as data, opens the confirmation, never downloads); its JS commands are not granted (lead approved) | MIT/Apache-2.0 | T-INF-12 |
| tauri-plugin-updater 2 (optional, `updater` feature; `rustls-tls` + `system-proxy`, no `zip`) | apps/desktop | signed self-update for direct downloads only ([docs/store/updates.md](store/updates.md)); minisign signature check, `requireSignedVersion`; MSIX and Flatpak builds leave the feature off (build.rs refuses it). Its `zip` feature is off: it only reads v1 update archives, and `zip` 4 has open advisories (RUSTSEC-2026-0367/0368/0369/0429). Called from Rust only, its JS commands are not granted (owner asked for it, lead approved) | MIT/Apache-2.0 | T-STORE-08 |
| tauri-plugin-single-instance 2 (`deep-link`) | apps/desktop | a link (or a second launch) opens the running app instead of a second copy; the second launch's command line is ignored, its link forwarded to the deep-link handler (lead approved) | MIT/Apache-2.0 | T-INF-12 |

### UI (npm, `apps/desktop/package.json`)

| Package | Why | Licence | Added in |
|---|---|---|---|
| react, react-dom 19 | UI framework (see the T-SHELL-01 PR for React vs Solid) | MIT | T-SHELL-01 |
| @tauri-apps/api ~2.11 | `invoke` + `Channel` for typed IPC; pinned to the Rust `tauri` minor | MIT/Apache-2.0 | T-SHELL-01 |
| marked | markdown → HTML for chat messages | MIT | T-SHELL-01 |
| dompurify | sanitises rendered model output | MPL-2.0 OR Apache-2.0 | T-SHELL-01 |
| highlight.js (core + 13 languages) | code highlighting | BSD-3-Clause | T-SHELL-01 |
| @radix-ui/react-{tooltip,switch,select,toast} | headless primitives required by the design standard (ADR-0010, §4) | MIT | T-SHELL-01 |
| @radix-ui/react-alert-dialog | shared confirm for irreversible actions (design-system §4 Dialog, alert variant; lead approved 2026-11-24) | MIT | T-STORE-11 |
| @radix-ui/react-dialog 1 | design-system Dialog for work in a layer above the page (the add-model dialog): role=dialog, focus trap, Esc, focus return, full screen on narrow windows; confirms stay on alert-dialog (lead approved 2026-11-25; already in the tree through alert-dialog) | MIT | T-SHELL-19 |
| @radix-ui/react-dropdown-menu 2 | design-system DropdownMenu (keyboard, typeahead, submenus, focus return); the sidebar chat menu: move to project, rename, delete (lead approved 2026-11-25) | MIT | T-SHELL-16 |
| lucide-react | icons, 16 px / 1.5 stroke (design standard §4) | ISC | T-SHELL-01 |
| dev: typescript 7, vite 8, @vitejs/plugin-react, vitest, @testing-library/react, jsdom, @types/react(-dom), @types/node 22, @tauri-apps/cli ~2.11 | strict typecheck, build, unit tests, `tauri dev/build` | MIT / Apache-2.0 | T-SHELL-01 |

### Licence tooling (T-STORE-07; lead approved 2026-11-24)

| Tool | Where | Why | Licence |
|---|---|---|---|
| cargo-about 0.9.3 (`--features cli`) | CI job `licences`, release builds; not a crate dependency | licence texts for the app's Rust graph (`about.toml`, accepted list mirrors deny.toml) → Settings › About › Open-source licences | MIT OR Apache-2.0 |
| `apps/desktop/scripts/npm-licenses.mjs`, `licenses.mjs` | our code, zero dependencies | npm production licences, hand-listed bundled C/C++ (`licenses-extra.json`), validation, `ui/src/generated/licenses.json` (gitignored; bundled as a lazy module, no fetch) | Apache-2.0 (this repo) |

Release builds (`tauri build` without `--debug`, i.e. `TAURI_ENV_DEBUG=false`, or `DEMIOS_REQUIRE_LICENSES=1`) fail if cargo-about is missing. Dev, test and debug builds skip the list with a warning.

### Release signing (T-REL-02, #120; CI tools only, lead approved 2026-11-25)

| Tool | Where | Why | Licence |
|---|---|---|---|
| trusted-signing-cli 0.11.0 ([levminer/trusted-signing-cli](https://github.com/levminer/trusted-signing-cli)), `cargo install --locked --version 0.11.0` | Windows job of the release workflow only; not a crate dependency, never in PR builds | Tauri's `bundle.windows.signCommand`: signs the exe and the NSIS installer with Azure Artifact Signing (formerly Trusted Signing), reading `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_CLIENT_SECRET` from the `release` environment | MIT |
| `scripts/release/signing-check.mjs`, `latest-json.mjs` | our code, zero dependencies | which platforms are signed (labelled `-unsigned` otherwise; a half-set-up account fails); the updater's `latest.json` from signed bundles only | Apache-2.0 (this repo) |

### Flatpak packaging (T-STORE-06; build tools only, not dependencies of any crate)

| Tool | Where | Why | Licence |
|---|---|---|---|
| flatpak-builder-tools (pinned commit in `generate-sources.sh`) with its Python requirements aiohttp and tomlkit (in a throwaway venv) | `packaging/flatpak/generate-sources.sh`, CI job `flatpak` | offline cargo and npm source lists for the Flatpak build | MIT (tools), Apache-2.0 (aiohttp), MIT (tomlkit) |
| org.flatpak.Builder (Flathub; flatpak-builder, flatpak-builder-lint, appstreamcli, desktop-file-validate) | CI job `flatpak`, local builds | builds the Flatpak and runs Flathub's linters | LGPL-2.1+ / MIT |
| cargo-about 0.9.3, built from its crates.io source inside the Flatpak build and removed from the app | `packaging/flatpak/io.demios.app.yml` | the licences list in an offline build (same tool and version as above) | MIT OR Apache-2.0 |

### e2e (`apps/desktop/e2e`, dev only)

| Package | Why | Licence | Added in |
|---|---|---|---|
| @wdio/cli, @wdio/local-runner, @wdio/mocha-framework, @wdio/spec-reporter, @wdio/globals 9.33.1 | WebdriverIO runner for the real-app tests (v9: Node 22 LTS, classic WebDriver for tauri-driver) | MIT | T-SHELL-05 |
| axe-core 4.14.1 | accessibility check on the chat screen (injected via WebDriver) | MPL-2.0 | T-SHELL-05 |
| tauri-driver 2.0.6 (CI tool, `cargo install --locked`, not a dependency of any crate) | WebDriver server that launches the Tauri app | Apache-2.0 OR MIT | T-SHELL-05 |
| msedgedriver (CI tool, Windows; runner's copy or Microsoft's download matching WebView2) | native WebDriver for WebView2 | Microsoft | T-SHELL-05 |
| Khronos Vulkan-Loader v1.4.368 (commit `f61c41f6b4c0a2fa3d2b900ab690febbf3235172`; **shipped**: `vulkan-1.dll` next to the exe in the Windows installer, `libvulkan.so.1` in the AppImage) | lets the Vulkan build start on machines without a Vulkan runtime and use the GPU where a driver is installed (T-INF-24). Built from source by `packaging/vulkan-loader/build.sh`, which refuses a tag that doesn't resolve to the pinned commit and records the built file's SHA-256 (release job summary, `target/vulkan-loader/SOURCE.txt`); the installer smoke tests check the installed copy against it. Windows loads it from the app's own folder only (an exe's linked DLLs are searched there first). deb and rpm depend on the system loader instead (`libvulkan1`, `vulkan-loader`). Listed on the licences screen | Apache-2.0 | T-INF-24 |
| LunarG Vulkan SDK 1.4.363.1 (CI tool, Windows and Linux; installer/tarball SHA-256 pinned in `vulkan.yml` and `release-preview.yml`) | builds llama.cpp's Vulkan backend (`glslc`, headers); not shipped | Apache-2.0 and others (build tools) | T-INF-24 |

Pre-approved, add a row when first used: tauri 2, tokio, serde_yaml (or a maintained successor), llama-cpp-2, rmcp, rusqlite (chosen over sqlx: sync, SQLCipher + sqlite-vec support), keyring, reqwest, axum (team server).

## npm (`ui/artifact-sandbox`)

Exact versions pinned in `package.json` and `package-lock.json`. Runtime libraries are bundled into the app (no CDN at runtime, ADR-0004); a licence scan of everything bundled is in the T-ART-02 PR.

| Package | Kind | Why | Licence | Added in |
|---|---|---|---|---|
| react, react-dom 19.3.1 | bundled | runtime for React artifacts (T-ART-04) | MIT | T-ART-04 |
| sucrase 3.35.1 | bundled | in-frame JSX/TS transpiler for React artifacts; chosen over esbuild-wasm (pure JS, no wasm CSP, small) | MIT | T-ART-04 |
| marked 18.3.0 | bundled | Markdown artifacts | MIT | T-ART-02 |
| mermaid 12.3.0 | bundled, lazy chunk (≈3.7 MB) | Mermaid artifacts. **Heavy**: loaded only when a diagram is shown. Its optional `elkjs` dependency (EPL-2.0 OR GPL-3.0) is stubbed out of the bundle; other transitive deps are MIT/ISC/BSD-3-Clause/Apache-2.0, plus dompurify (MPL-2.0 OR Apache-2.0, used under Apache-2.0) and robust-predicates (Unlicense) | MIT | T-ART-02 |
| esbuild 0.28.2 | dev | bundles runtimes and the host library | MIT | T-ART-02 |
| typescript 5.9.3 | dev | strict type-check | Apache-2.0 | T-ART-02 |
| @playwright/test 1.57.0 | dev | UI flow and sandbox escape tests in Chromium + WebKit | Apache-2.0 | T-ART-05 |
| @types/node, @types/react, @types/react-dom | dev | types | MIT | T-ART-02 |

