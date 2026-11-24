# Dependencies

Every direct dependency is listed here with why and its licence (ADR-0009). Approved set: D-012. Anything not in the approved set needs the lead session's OK before the PR.

| Crate | Used by | Why | Licence | Added in |
|---|---|---|---|---|
| serde, serde_json | all | (de)serialisation of contracts | MIT/Apache-2.0 | T-CORE-01 |
| thiserror | all | typed errors | MIT/Apache-2.0 | T-CORE-01 |
| uuid (v7) | dn-core | time-ordered, sync-safe ids | MIT/Apache-2.0 | T-CORE-01 |
| futures-core | dn-core | `Stream` trait for `ChatModel::chat` | MIT/Apache-2.0 | T-CORE-01 |
| rusqlite 0.43 (`bundled`) | dn-store | SQLite store and migration runner (ADR-0007); bundled SQLite so all three OSes build the same | MIT (SQLite: public domain) | T-CORE-02 |
| tokio | dn-inference, dn-models, dn | async runtime, channels, fs for downloads | MIT | T-INF-01 |
| llama-cpp-2 (+ llama-cpp-sys-2, builds llama.cpp) | dn-inference | in-process llama.cpp backend; `metal` on macOS, `cuda`/`vulkan` features | MIT/Apache-2.0 (llama.cpp: MIT) | T-INF-02 |
| reqwest (rustls) | dn-inference, dn-models | OpenAI-compatible streaming; model downloads. Pulls `webpki-root-certs` (CDLA-Permissive-2.0, exception in `deny.toml`) | MIT/Apache-2.0 | T-INF-05, T-INF-06 |
| sha2 | dn-models | SHA-256 verification of downloads | MIT/Apache-2.0 | T-INF-05 |
| sysinfo (`system` only) | dn-models, dn | RAM detection for the fit estimator; peak RSS in `dn bench` | MIT | T-INF-03 |
| clap (derive) | dn | `dn` command-line parsing | MIT/Apache-2.0 | T-INF-04 |

Approved for wave 1 (D-012), add a row when first used: tauri 2, tokio, serde_yaml (or a maintained successor), llama-cpp-2, rmcp, rusqlite (chosen over sqlx: sync, SQLCipher + sqlite-vec support), keyring, reqwest, axum (team server).
