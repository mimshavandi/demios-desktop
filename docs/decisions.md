# Decisions log

Each decision: ID, question, options, recommendation, status, decided-by/date. Status: `open` → `decided` (or `superseded by D-xxx`).

| ID | Question | Options | Recommendation | Status (2026-11-24) |
|---|---|---|---|---|
| D-001 | Product names | Keep Demios family (Demios Desktop / Demios Twin / Demios Team) · new brand · new name for OSS app only | Keep **Demios** family: brand already exists, Twin is a known concept; rename later is cheap before launch | decided |
| D-002 | Repo names + visibility | `demios-desktop` (public) + `demios-pro` (private) · one public monorepo · per-product repos | **`demios-desktop` public + `demios-pro` private** (ADR-0001) | decided |
| D-003 | Personal GitHub account for the repos | `mimshavandi` (the account `gh` is logged in as) · other | Confirm `mimshavandi` | decided |
| D-004 | Licences | A Apache-2.0 · B MIT · C AGPL · D Apache-2.0 client + FSL team server | **D**: Apache-2.0 desktop + crates; Twin proprietary; team server FSL-1.1-Apache-2.0 | decided |
| D-005 | Premium model for the Twin | Subscription licence key · one-time licence · hosted-only | Subscription licence key checked offline (signed licence file) | decided |
| D-006 | Desktop shell + UI language | Tauri 2 + TS (strict) · Tauri 2 + Leptos (Rust UI) · Dioxus · native egui | **Tauri 2 + strict TypeScript UI**, contracts generated from Rust (ADR-0002) | decided |
| D-007 | Inference engine | llama.cpp default + mistral.rs spike · mistral.rs only · FreeToken | **llama.cpp default**, mistral.rs evaluated by benchmark, FreeToken/Ollama as external OpenAI-compatible backends (ADR-0003, R-01) | decided |
| D-008 | Which workers start first | Wave 1 as in workers.md (5 workers) · fewer (inference + shell first) | Lead does wave 0, then **all 5 wave-1 workers** in parallel | decided |
| D-009 | Team server stack | Rust axum + Postgres single binary · reuse Demios services | **Rust single binary + Postgres** (ADR-0008) | decided |
| D-010 | E2E encryption for team sync | Yes (privacy, no server-side agents) · No (server can search/run agents) · per-space choice | Per-space choice, default off | decided |
| D-011 | MVP platform order | macOS first, Win/Linux compile-only · all three at MVP | **macOS first** (Demios Edge users, Metal) with Win/Linux CI builds | decided |
| D-012 | Core dependency set | Approve: tauri 2, tokio, serde/serde_yaml, llama-cpp-2, rmcp, rusqlite or sqlx, keyring, reqwest, axum (team) · review one by one | Approve the set; anything else asks the lead first | decided |
| D-013 | Codebase-conversion tooling | craft-anything · TrustGraph · direct Claude Code sessions per module | **Direct sessions against written contracts**; don't use craft-anything or TrustGraph for the port (R-01) | decided |
| D-014 | Visual direction of the design standard (ADR-0010) | Quiet neutral chrome + Demios indigo accent + mint for on-device, system fonts · other direction | As proposed in `docs/design/design-system.md`; preview shared for review | decided |

## Decided

**2026-11-24 — owner accepted every recommendation above** (D-001 … D-013), in reply to the lead session's decision list. Notes:
- D-002/D-003: repos `mimshavandi/demios-desktop` (public) and `mimshavandi/demios-pro` (private). Demios-specific research (source survey, reuse map, which cites Demios internals) lives only in `demios-pro`; public docs were checked for Demios security details before the first push.
- D-008: lead session does wave 0 (T-GOV-01, T-GOV-02, T-CORE-01), then starts the five wave-1 workers.
- D-012: dependency set approved; additions go through the lead and `docs/dependencies.md`.

**2026-11-24 — owner approved D-014** (visual direction as in `docs/design/preview.html`); ADR-0010 accepted.
