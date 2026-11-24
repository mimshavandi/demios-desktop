# Roadmap

Phases are gated by exit criteria, not dates. Durations are rough sizing for planning, assuming 4–6 parallel worker sessions plus review by the lead session and the owner.

## Phase 0 — Foundations (≈1 week)
Repos created (after D-002), Cargo workspace skeleton (ADR-0001), CI with fmt/clippy/test/deny/audit + gitleaks (ADR-0009), `dn-core` types, docs moved into the repo.
**Exit:** `cargo test --workspace` green in CI on macOS, Windows, Linux; gitleaks job passes; `docs/` published in repo.

## Phase 1 — Product 1 MVP: "chat with your own model" (≈6–8 weeks)
Scope (macOS first, Windows + Linux builds compiling — D-011):
- Chat UI with streaming, conversation history, markdown/code rendering, stop/regenerate.
- **Local models:** curated catalogue (≈10 GGUF models), download with verification, fit estimator recommending quant/context/offload, llama.cpp backend with Metal/CUDA/CPU, idle unload.
- **BYOK:** OpenAI, Anthropic, Gemini, any OpenAI-compatible URL (covers Ollama/LM Studio/FreeToken). Keys in keychain.
- **Artifacts v1:** html, svg, mermaid, markdown, code; sandboxed renderer; versions + rollback; create/update/rewrite tools + fenced fallback.
- **Tools:** MCP client (stdio + HTTP), approvals, side-effect ledger.
- `dn` CLI for headless runs and benchmarks.
**Exit (MVP done-check):** on a 16 GB Apple Silicon laptop, a new user installs the app, downloads a recommended model, chats with streaming, asks for "a pomodoro timer as an HTML artifact", sees it render, asks for a change, gets version 2, rolls back to version 1, connects one MCP server (filesystem) and approves a tool call — with no account and no network after the model download. Same flow works with an Anthropic or OpenAI key.

## Phase 2 — Product 1 v1.0 public release (≈6 weeks)
React artifacts; projects (files + instructions); local knowledge index over files (hybrid search); declarative skills + skill library ported from Demios prompts; mistral.rs decision from benchmarks; MoE-offload presets for 30B-class MoE models on 16–32 GB machines; Windows + Linux parity; signed/notarised releases + updater; contributor docs; public launch.
**Exit:** benchmark report published (tokens/s, memory, quality) on 8/16/32 GB machines; release artifacts signed; zero high-severity findings in sandbox threat model.

## Phase 3 — Product 2 alpha: Twin executive assistant (≈8 weeks, can start mid-Phase 2)
Plugin API (`dn-plugin-api`) in OSS core; Twin crate in private repo: persona + context envelope, governed memory (candidate queue, explainable recall), risk classifier, task ledger with approval hold bound to the tools ledger, heartbeat (morning brief, quiet hours, digest), mail/calendar/tasks via MCP connectors; Demios Twin eval datasets running in CI; licence-key unlock.
**Exit:** 10 design-partner users run a daily brief + triage + drafted replies for 2 weeks; zero unapproved external writes in the ledger; eval pass-rate target set in D-012.

## Phase 4 — Product 3 alpha: Team (≈8 weeks)
Team server (auth, orgs, invites, audit), CRDT sync of opted-in spaces, shared artifacts/skills, shared model proxy, Docker image.
**Exit:** 3 devices across 2 users edit the same space offline and converge; revoke-device works; server runs from one `docker run`.

## Later
OIDC SSO, mobile approvals app (Tauri mobile), more artifact types (slides, sheets, boards — Demios designers reborn), hosted team server, Twin voice.
