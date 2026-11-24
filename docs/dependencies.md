# Dependencies

Every direct dependency is listed here with why and its licence (ADR-0009). Approved set: D-012. Anything not in the approved set needs the lead session's OK before the PR.

| Crate | Used by | Why | Licence | Added in |
|---|---|---|---|---|
| serde, serde_json | all | (de)serialisation of contracts | MIT/Apache-2.0 | T-CORE-01 |
| thiserror | all | typed errors | MIT/Apache-2.0 | T-CORE-01 |
| uuid (v7) | dn-core | time-ordered, sync-safe ids | MIT/Apache-2.0 | T-CORE-01 |
| futures-core | dn-core | `Stream` trait for `ChatModel::chat` | MIT/Apache-2.0 | T-CORE-01 |
| keyring 3 | dn-secrets | OS keychain for BYOK keys and MCP tokens (D-012, ADR-0007). Features `apple-native`, `windows-native`, `async-secret-service` + `tokio` + `crypto-rust`, so there are no C deps (libdbus/openssl) | MIT/Apache-2.0 | T-PRV-03 |

Approved for wave 1 (D-012), add a row when first used: tauri 2, tokio, serde_yaml (or a maintained successor), llama-cpp-2, rmcp, rusqlite (chosen over sqlx: sync, SQLCipher + sqlite-vec support), keyring, reqwest, axum (team server).
