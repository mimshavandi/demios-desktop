# Dependencies

Every direct dependency is listed here with why and its licence (ADR-0009). Approved set: D-012. Anything not in the approved set needs the lead session's OK before the PR.

| Crate | Used by | Why | Licence | Added in |
|---|---|---|---|---|
| serde, serde_json | all | (de)serialisation of contracts | MIT/Apache-2.0 | T-CORE-01 |
| thiserror | all | typed errors | MIT/Apache-2.0 | T-CORE-01 |
| uuid (v7) | dn-core | time-ordered, sync-safe ids | MIT/Apache-2.0 | T-CORE-01 |
| futures-core | dn-core | `Stream` trait for `ChatModel::chat` | MIT/Apache-2.0 | T-CORE-01 |
| tokio | dn-agent | async runtime; `sync` for cancellation (D-012) | MIT | T-AGT-02 |
| serde_yaml_ng 0.10 | dn-agent | YAML manifests (`agent.yaml`, `skill.yaml`, prompt `manifest.yaml`). Maintained fork of the archived `serde_yaml` (D-012: "serde_yaml or a maintained successor") | MIT | T-AGT-01 |
| jsonschema 0.63 (`default-features = false`) | dn-agent | validate manifests against bundled Draft 2020-12 schemas; no remote/file `$ref` resolution (approved by lead) | MIT | T-AGT-01 |
| rusqlite 0.43 (`bundled`) | dn-store | SQLite store and migration runner (ADR-0007); bundled SQLite so all three OSes build the same | MIT (SQLite: public domain) | T-CORE-02 |

Approved for wave 1 (D-012), add a row when first used: tauri 2, tokio, serde_yaml (or a maintained successor), llama-cpp-2, rmcp, rusqlite (chosen over sqlx: sync, SQLCipher + sqlite-vec support), keyring, reqwest, axum (team server).
