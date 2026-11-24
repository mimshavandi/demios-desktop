# demios-next

Planning home for splitting Demios into three Rust products. This folder becomes the `docs/` of the `demios-desktop` repo once D-002 is decided. Nothing here has been pushed anywhere.

| Doc | What it answers |
|---|---|
| [docs/vision.md](docs/vision.md) | The three products, scope, free vs premium, licence options |
| [docs/adr/](docs/adr/README.md) | Architecture decisions (shell, inference, artifacts, agents, tools, storage, team sync, security) |
| [docs/roadmap.md](docs/roadmap.md) | Phases with exit criteria; Product 1 MVP first |
| [docs/workers.md](docs/workers.md) | Worker sessions, task IDs, done-checks, brief template |
| [docs/decisions.md](docs/decisions.md) | Open decisions (D-001…D-013) and their outcomes |
| [docs/traceability.md](docs/traceability.md) | IDs, branch/commit/PR conventions |
| [docs/research/](docs/research/) | R-01 external tools (the Demios source survey and reuse map live in the private `demios-pro` repo) |

## Architecture at a glance

```
            ┌───────────────────── apps/desktop (Tauri 2) ─────────────────────┐
            │  UI (chat, settings, artifacts pane)   │ sandboxed artifact view │
            └───────────────┬──────────────────────────────────────────────────┘
                            │ typed IPC (contracts from dn-core)
  ┌────────────┬────────────┼─────────────┬──────────────┬─────────────┐
  dn-agent     dn-artifacts dn-tools      dn-knowledge   dn-store/dn-secrets
  (loop,       (versions,   (MCP client,  (FTS+vectors)  (SQLite, keychain)
   skills,      patch tools) risk, ledger)
   prompts)
     │
  dn-providers (BYOK/remote)  ─┐
  dn-inference (llama.cpp ▸ mistral.rs ▸ openai-compat) ─ dn-models (catalogue, fit)
                               │
  premium: twin crate (via dn-plugin-api) · team: dn-team-server ⇄ dn-sync
```

Predecessor: Demios (private). Concepts are ported as written contracts, never as copied code or history.
