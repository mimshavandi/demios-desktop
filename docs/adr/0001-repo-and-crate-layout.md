# ADR-0001: Repo and crate layout

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner (D-002 repo names/visibility)

## Context
Three products share one base. Product 1 is open source; the Twin module and possibly the team server are not. Demios suffered from copied helpers across 11 services with no shared package; we want one source of truth per concern from day one.

## Decision
Two repos, one Cargo workspace each:

1. **`demios-desktop`** (public, Apache-2.0) — the open-source app and every shared crate:
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
     dn-sync        (client half of team sync; feature-gated)
     dn-plugin-api  stable trait surface the proprietary Twin module plugs into
   apps/
     desktop        Tauri 2 app (Rust backend + web UI in apps/desktop/ui)
     cli            `dn` CLI: run models, run skills, benchmark (useful for tests and CI)
   skills/          built-in declarative skills
   evals/           eval datasets and harness
   docs/            these docs move here
   ```
2. **`demios-pro`** (private) — `twin` crate(s) implementing `dn-plugin-api`, the team server (`dn-team-server`), licensing, and the premium build of the app that links them. Depends on `demios-desktop` crates by git tag.

## Options considered

| Option | Complexity | OSS hygiene | Cross-cutting change cost |
|---|---|---|---|
| A. One public monorepo, premium code behind features | Low | Bad — premium source is public | Lowest |
| **B. Public core repo + private pro repo** | Medium | Good | Medium (version bumps) |
| C. One repo per product (3+) | High | Good | High; reintroduces copying |

## Trade-offs
B keeps the licence boundary physical and auditable. Cost: premium changes needing core changes become two PRs; mitigated by keeping `dn-plugin-api` small and stable.

## Consequences
- Contributors only ever see `demios-desktop`.
- The premium app is a different build target, not a runtime unlock hiding code in the OSS binary.
- Revisit if the team server goes open source (D-004): it can then move into the public repo.

## Action items
- T-GOV-01 create repos (after D-002), T-GOV-02 workspace skeleton + CI, T-CORE-01 `dn-core` types.
