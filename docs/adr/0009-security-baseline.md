# ADR-0009: Security and supply-chain baseline

**Status:** Proposed · **Date:** 2026-11-24

## Context
The predecessor codebase (Demios) carries history and delivery practices we do not want to inherit. The new repos must start clean, with secret scanning and tests gating every change from the first commit.

## Decision
- New repos start with **fresh history**; no `git subtree`/filter-branch import from Demios. Ported content enters as new files reviewed in PRs.
- **gitleaks** pre-commit hook + CI job on every PR from commit 1 (`.gitleaks.toml` with no allowlist by default).
- CI gates on every PR: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `cargo deny` (licences + advisories), `cargo audit`, UI typecheck (strict) + tests. Release builds only from tags after CI.
- `#![forbid(unsafe_code)]` in all crates except the FFI boundary in `dn-inference`.
  - Also excepted since 2026-11-26 (owner approved): the phone crate `dn-mobile`, for UniFFI-generated bindings only, checked in CI ([ADR-0019](0019-iphone-app.md) §2).
- Signed, notarised releases (Apple notarisation, Windows Authenticode), Tauri updater with signature verification.
- No telemetry by default; opt-in only and documented.
- New dependencies need a line in `docs/dependencies.md` (why, licence) — mirrors the Demios "ask before adding dependencies" rule.
- Artifact sandbox and MCP tool execution get dedicated threat models (T-ART-05, T-CON-05).
