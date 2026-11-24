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
| tokio | dn-inference, dn-models, dn | async runtime, channels, fs for downloads | MIT | T-INF-01 |
| llama-cpp-2 (+ llama-cpp-sys-2, builds llama.cpp) | dn-inference | in-process llama.cpp backend; `metal` on macOS, `cuda`/`vulkan` features | MIT/Apache-2.0 (llama.cpp: MIT) | T-INF-02 |
| reqwest 0.13 (rustls) | dn-models | resumable model downloads (`json` feature for the catalogue API is not needed) | MIT/Apache-2.0 | T-INF-05 |
| sha2 | dn-models | SHA-256 verification of downloads | MIT/Apache-2.0 | T-INF-05 |
| sysinfo (`system` only) | dn-models, dn | RAM detection for the fit estimator; peak RSS in `dn bench` | MIT | T-INF-03 |
| clap (derive) | dn | `dn` command-line parsing | MIT/Apache-2.0 | T-INF-04 |

Approved for wave 1 (D-012), add a row when first used: tauri 2, tokio, serde_yaml (or a maintained successor), llama-cpp-2, rmcp, rusqlite (chosen over sqlx: sync, SQLCipher + sqlite-vec support), keyring, reqwest, axum (team server).
