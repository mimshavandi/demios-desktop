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
| rusqlite 0.43 (`bundled`) | dn-store | SQLite store and migration runner (ADR-0007); bundled SQLite so all three OSes build the same | MIT (SQLite: public domain) | T-CORE-02 |

Approved for wave 1 (D-012), add a row when first used: tauri 2, tokio, serde_yaml (or a maintained successor), llama-cpp-2, rmcp, rusqlite (chosen over sqlx: sync, SQLCipher + sqlite-vec support), keyring, reqwest, axum (team server).
