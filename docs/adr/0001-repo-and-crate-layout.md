# ADR-0001: Repo and crate layout

**Status:** Proposed · **Date:** 2026-11-24 (amended 2026-11-26: `dn-jobs`; 2026-11-27: `dn-web`, `dn-licence` (ADR-0014 §9a); 2026-11-28: `dn-runtime`, ADR-0020; `dn-data`; `dn-memory`, ADR-0020 §4) · **Deciders:** project owner

## Context
The open-source app and any proprietary add-ons share one base. The app and every shared crate are public; proprietary add-ons live in a separate private repository. Demios suffered from copied helpers across 11 services with no shared package; we want one source of truth per concern from day one.

## Decision
Two repos, one Cargo workspace each:

1. **`demios-desktop`** (public, PolyForm Shield 1.0.0 since ADR-0017; Apache-2.0 before) — the source-available app and every shared crate:
   ```
   crates/
     dn-core        domain types: ids, model refs, slots, conversation, artifact, events
     dn-inference   InferenceBackend trait + llama.cpp backend (+ mistral.rs spike)
     dn-models      model catalogue, download, verification, fit estimation
     dn-providers   BYOK/remote providers (OpenAI, Anthropic, Gemini, OpenAI-compatible) + gateway policies
     dn-agent       skill/agent manifests, prompt registry, tool-use loop, executor
     dn-tools       MCP client, capability layer, risk + approval, side-effect ledger
     dn-artifacts   artifact store, versions, update protocol
     dn-knowledge   local index (FTS + vectors), ingestion, embedding
     dn-store       SQLite schema + migrations
     dn-secrets     keychain access
     dn-licence     the signed, offline licence file and its check (ADR-0013 §6, ADR-0014 §9a; added with the owner's approval, 2026-11-27)
     dn-sync        sync client (feature-gated)
     dn-plugin-api  stable trait surface that add-ons plug into
     dn-jobs        background-job scheduler, missed-run evaluation (dn-plugin-api's rules), held-approvals queue, run history; used by the app and the Node
     dn-data        tabular files (CSV, XLSX, XLS, ODS) to a computed fact sheet; the model quotes it and never calculates (#366)
     dn-web         web search providers (the user's own keys or SearXNG) and the local page fetcher: private-network guard, robots.txt, extraction to markdown; no Demios proxy (#369)
     dn-memory      the memory store: items with scope, trust and sources, the pending queue, purge by source (ADR-0020 §4; added with the owner's approval, 2026-11-28)
     dn-runtime     the agent runtime around dn-agent's loop: context assembler (token budgets, fixed slots, drop log), compaction; every model call goes through it (ADR-0020)
   apps/
     desktop        Tauri 2 app (Rust backend + web UI in apps/desktop/ui)
     cli            `dn` CLI: run models, run skills, benchmark (useful for tests and CI)
   skills/          built-in declarative skills
   evals/           eval datasets and harness
   docs/            contributor docs
   ```
2. **A private repository** for proprietary add-ons, which plug in through `dn-plugin-api`. It depends on `demios-desktop` crates by git tag.

## Options considered

| Option | Complexity | OSS hygiene | Cross-cutting change cost |
|---|---|---|---|
| A. One public monorepo, proprietary code behind features | Low | Bad — proprietary source is public | Lowest |
| **B. Public core repo + private add-on repo** | Medium | Good | Medium (version bumps) |
| C. One repo per component (3+) | High | Good | High; reintroduces copying |

## Trade-offs
B keeps the licence boundary physical and auditable. Cost: add-on changes needing core changes become two PRs; mitigated by keeping `dn-plugin-api` small and stable.

## Consequences
- Contributors only ever see `demios-desktop`.
- A build with add-ons is a different build target, not a runtime unlock hiding code in the open-source binary.
- Revisit if an add-on becomes open source: it can then move into the public repo.

## Action items
- T-GOV-01 create repos, T-GOV-02 workspace skeleton + CI, T-CORE-01 `dn-core` types.
