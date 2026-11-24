# Dependencies

Every direct dependency is listed here with why and its licence (ADR-0009). Approved set: D-012. Anything not in the approved set needs the lead session's OK before the PR.

| Crate | Used by | Why | Licence | Added in |
|---|---|---|---|---|
| serde, serde_json | all | (de)serialisation of contracts | MIT/Apache-2.0 | T-CORE-01 |
| thiserror | all | typed errors | MIT/Apache-2.0 | T-CORE-01 |
| uuid (v7) | dn-core | time-ordered, sync-safe ids | MIT/Apache-2.0 | T-CORE-01 |
| futures-core | dn-core | `Stream` trait for `ChatModel::chat` | MIT/Apache-2.0 | T-CORE-01 |
| ts-rs 12 (optional, `ts` feature; `serde-compat`, no default features) | dn-core | TypeScript bindings for the UI in `crates/dn-core/bindings/`; not in the default build | MIT | T-CORE-04 |
| tokio | dn-agent | async runtime; `sync` for cancellation (D-012) | MIT | T-AGT-02 |
| rusqlite 0.43 (`bundled`) | dn-store | SQLite store and migration runner (ADR-0007); bundled SQLite so all three OSes build the same | MIT (SQLite: public domain) | T-CORE-02 |

Approved for wave 1 (D-012), add a row when first used: tauri 2, tokio, serde_yaml (or a maintained successor), llama-cpp-2, rmcp, rusqlite (chosen over sqlx: sync, SQLCipher + sqlite-vec support), keyring, reqwest, axum (team server).
