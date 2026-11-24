# Dependencies

Every direct dependency is listed here with why and its licence (ADR-0009). Approved set: D-012. Anything not in the approved set needs the lead session's OK before the PR.

| Crate | Used by | Why | Licence | Added in |
|---|---|---|---|---|
| serde, serde_json | all | (de)serialisation of contracts | MIT/Apache-2.0 | T-CORE-01 |
| thiserror | all | typed errors | MIT/Apache-2.0 | T-CORE-01 |
| uuid (v7) | dn-core | time-ordered, sync-safe ids | MIT/Apache-2.0 | T-CORE-01 |
| futures-core | dn-core | `Stream` trait for `ChatModel::chat` | MIT/Apache-2.0 | T-CORE-01 |
| keyring 3 | dn-secrets | OS keychain for BYOK keys and MCP tokens (D-012, ADR-0007). Features `apple-native`, `windows-native`, `async-secret-service` + `tokio` + `crypto-rust`, so there are no C deps (libdbus/openssl) | MIT/Apache-2.0 | T-PRV-03 |
| tokio | dn-agent, dn-secrets, dn-providers | async runtime; `sync` for streams/cancellation, `rt` for `AsyncSecrets::spawn_blocking` (D-012) | MIT | T-AGT-02 |
| reqwest 0.13 | dn-providers | HTTPS client for BYOK providers (D-012). `rustls` (aws-lc-rs, platform verifier) + `http2` + `charset`; no OpenSSL. SSE parsing is hand-rolled (`dn_providers::sse`) | MIT/Apache-2.0 | T-PRV-01 |
| zeroize 1 | dn-secrets | wipe `Secret` buffers on drop (`Zeroizing<String>`); already a transitive dep of keyring | MIT/Apache-2.0 | T-PRV-03 |
| ts-rs 12 (optional, `ts` feature; `serde-compat`, no default features) | dn-core | TypeScript bindings for the UI in `crates/dn-core/bindings/`; not in the default build | MIT | T-CORE-04 |
| serde_yaml_ng 0.10 | dn-agent | YAML manifests (`agent.yaml`, `skill.yaml`, prompt `manifest.yaml`). Maintained fork of the archived `serde_yaml` (D-012: "serde_yaml or a maintained successor") | MIT | T-AGT-01 |
| jsonschema 0.63 (`default-features = false`) | dn-agent | validate manifests against bundled Draft 2020-12 schemas; no remote/file `$ref` resolution (approved by lead) | MIT | T-AGT-01 |
| rusqlite 0.43 (`bundled`) | dn-store, dn-tools | SQLite store and migration runner (ADR-0007); bundled SQLite so all three OSes build the same. dn-tools: side-effect ledger (`tool_ledger_*`) | MIT (SQLite: public domain) | T-CORE-02 |
| rmcp 3.11 (client, transport-async-rw, streamable-http-client-reqwest, auth) | dn-tools | MCP client over stdio + Streamable HTTP; OAuth (PKCE, discovery, DCR) via its `auth` feature (pulls `oauth2`, `url`). Dev: `server`, `transport-io`, `transport-streamable-http-server` for test servers | Apache-2.0 | T-CON-01 |
| tokio | dn-tools | async runtime, child processes, timeouts, approval channels | MIT | T-CON-01 |
| reqwest 0.13 (rustls) | dn-tools | HTTP client handed to rmcp (no redirects for MCP traffic) | MIT/Apache-2.0 | T-CON-01 |
| axum 0.8 (dev only) | dn-tools tests | in-process Streamable HTTP + OAuth test servers | MIT | T-CON-01 |
| process-wrap 10 (tokio1, process-group, job-object) | dn-tools | stdio MCP servers run in their own process group (Unix) / job object (Windows) so shutdown also stops grandchildren (e.g. npx → node); same major version rmcp already uses | Apache-2.0 OR MIT | T-CON-01 |
| tauri 2, tauri-build 2 | apps/desktop | desktop shell, typed IPC, capabilities (ADR-0002, D-006) | MIT/Apache-2.0 | T-SHELL-01 |
| tokio (`sync`, `macros`, `time`, `rt`) | apps/desktop | cancel a streaming answer (`select!` + `oneshot`); timer for the dev fake model | MIT | T-SHELL-01 |

### UI (npm, `apps/desktop/package.json`)

| Package | Why | Licence | Added in |
|---|---|---|---|
| react, react-dom 19 | UI framework (see the T-SHELL-01 PR for React vs Solid) | MIT | T-SHELL-01 |
| @tauri-apps/api ~2.11 | `invoke` + `Channel` for typed IPC; pinned to the Rust `tauri` minor | MIT/Apache-2.0 | T-SHELL-01 |
| marked | markdown → HTML for chat messages | MIT | T-SHELL-01 |
| dompurify | sanitises rendered model output | MPL-2.0 OR Apache-2.0 | T-SHELL-01 |
| highlight.js (core + 13 languages) | code highlighting | BSD-3-Clause | T-SHELL-01 |
| @radix-ui/react-{tooltip,switch,select,toast} | headless primitives required by the design standard (D-014, §4) | MIT | T-SHELL-01 |
| lucide-react | icons, 16 px / 1.5 stroke (design standard §4) | ISC | T-SHELL-01 |
| dev: typescript 7, vite 8, @vitejs/plugin-react, vitest, @testing-library/react, jsdom, @types/react(-dom), @types/node 22, @tauri-apps/cli ~2.11 | strict typecheck, build, unit tests, `tauri dev/build` | MIT / Apache-2.0 | T-SHELL-01 |

### e2e (`apps/desktop/e2e`, dev only; D-015)

| Package | Why | Licence | Added in |
|---|---|---|---|
| @wdio/cli, @wdio/local-runner, @wdio/mocha-framework, @wdio/spec-reporter, @wdio/globals 9.33.1 | WebdriverIO runner for the real-app tests (v9: Node 22 LTS, classic WebDriver for tauri-driver) | MIT | T-SHELL-05 |
| axe-core 4.14.1 | accessibility check on the chat screen (injected via WebDriver) | MPL-2.0 | T-SHELL-05 |
| tauri-driver 2.0.6 (CI tool, `cargo install --locked`, not a dependency of any crate) | WebDriver server that launches the Tauri app | Apache-2.0 OR MIT | T-SHELL-05 |
| msedgedriver (CI tool, Windows; runner's copy or Microsoft's download matching WebView2) | native WebDriver for WebView2 | Microsoft | T-SHELL-05 |

Approved for wave 1 (D-012), add a row when first used: tauri 2, tokio, serde_yaml (or a maintained successor), llama-cpp-2, rmcp, rusqlite (chosen over sqlx: sync, SQLCipher + sqlite-vec support), keyring, reqwest, axum (team server).
