# Worker breakdown

The lead session plans, writes briefs, reviews every PR against its done-check, and is the only one to mark a task `done`. Workers report to the lead, not to the owner. One worker = one session = one set of crates (no two writers on the same files).

## Wave 0 (lead session, after D-002)
| Task | What | Doc | Done-check |
|---|---|---|---|
| T-GOV-01 | Create `demios-desktop` (public) and `demios-pro` (private) under the confirmed personal account | ADR-0001, D-002 | repos exist, branch protection on `main`, owner confirmed |
| T-GOV-02 | Workspace skeleton, CI (fmt, clippy, test, deny, audit, gitleaks) on 3 OSes, licence files, CONTRIBUTING, docs moved in | ADR-0001, ADR-0009 | CI green on an empty-crate PR; a PR containing a fake key is blocked by gitleaks |
| T-CORE-01 | `dn-core` shared types (ids, model refs, slots, ChatRequest/TokenStream, Artifact, Capability, errors) | ADR-0003/4/5/6 | `cargo test -p dn-core`; types exported to TS |

## Design (lead session)
| Task | What | Doc | Done-check |
|---|---|---|---|
| T-DES-01 | Design tokens package + written design standard | ADR-0010, docs/design/design-system.md | `node ui/design-tokens/scripts/build.mjs --check` passes in CI (all contrast pairs, both themes); owner approves D-014 |
| T-DES-02 | stylelint ban on raw values + behaves audit in CI | ADR-0010 §8 | a PR with a raw hex colour in `apps/desktop/ui` fails CI |

## Wave 1 — Product 1 MVP (parallel after wave 0)
| Worker | Tasks | Crates/dirs | Done-check (summary) |
|---|---|---|---|
| **W-inference** | T-INF-01..06 | `dn-inference`, `dn-models`, `apps/cli` | `dn run --model <gguf> "hi"` streams on macOS Metal; `dn fit <model>` matches the Swift estimator on the ported test cases; `dn bench` report produced |
| **W-providers** | T-PRV-01..03, T-AGT-01..03 | `dn-providers`, `dn-agent`, `dn-secrets` | BYOK chat streams for OpenAI/Anthropic/Gemini/OpenAI-compat (recorded fixtures in tests); resolver returns `ModelSourceRequired` with recovery actions when nothing is configured; agent loop executes a 2-step tool chain in tests |
| **W-shell** | T-SHELL-01..03, T-CORE-02 | `apps/desktop`, `dn-store` | App launches on macOS; new chat → streaming answer from a local model; history persists across restart; model settings screen |
| **W-artifacts** | T-ART-01..05 | `dn-artifacts`, `apps/desktop/ui/artifacts` | create → render → update → v2 → rollback flow passes an automated UI test; sandbox escape test suite (no IPC, no network, no parent DOM) passes |
| **W-tools** | T-CON-01..05 | `dn-tools` | connects to the reference MCP filesystem + one HTTP server; read tool auto-runs, write tool waits for approval; ledger replay returns cached output |

Integration: the lead session merges wave 1 into the MVP flow and runs the Phase 1 exit check.

## Wave 2+ (briefs written when wave 1 lands)
W-knowledge (local index), W-skills-port (T-PORT-01: Demios prompts → declarative skills), W-release (signing, updater), W-twin (private repo), W-team (private repo).

## Brief template (every worker gets this)
1. Task IDs and the doc links that define them.
2. Files/crates it owns; files it must not touch.
3. Contracts it consumes (`dn-core` types) and exposes.
4. Done-check, verbatim.
5. Rules: branch `t/<task-id>-<slug>`, commit subjects start with the task ID, ask the lead before adding a dependency not listed in the ADR, gitleaks must pass, report to the lead session with PR link + done-check output.
6. Source references in Demios (read-only, cite path + commit; never copy secrets or history).
