# Vision and product split

Status: **draft for agreement** (open items are marked `D-###`, see [decisions.md](decisions.md)).
Working names below are placeholders until `D-001` is decided.

## Why split

Demios today is ~1.2M lines across 16 apps and 11 services: chat, multi-agent orchestration, a Twin, seven no-code designers, flows, 77 connectors, mail/calendar/channels, billing, an admin panel, mobile and a Mac runtime. It is too big to launch as one product, it needs a Kubernetes cluster to run, and key parts (the Twin's autonomy, approvals delivery) are unfinished.

The split keeps the part that is genuinely differentiated — **models belong to the user** — and ships it as a small, local-first desktop app first. Everything else is earned back feature by feature.

## The three products

| | 1. Personal (open source) | 2. Executive assistant (premium) | 3. Team |
|---|---|---|---|
| Working name | **Demios Desktop** | **Demios Twin** | **Demios Team** |
| Who | Individuals, developers, privacy-minded users | Executives, founders, busy professionals | Small teams (2–50) |
| Shape | Claude-style desktop chat app | Same app + an always-on assistant that acts for you | Same app + a self-hostable central server |
| Models | Local (built-in engine) or BYOK | Same | Same; team may share BYOK keys/models via the server |
| Core loop | Chat → tools → artifacts | Inbox/calendar/tasks → Twin proposes → you approve → Twin acts | Shared projects, artifacts, agents and knowledge |
| Runs where | 100% on the user's machine | User's machine; optional relay for mobile/approvals | Desktop + one server (Docker / single binary) |
| Licence | Open source (`D-004`) | Proprietary add-on | Open-core: client OSS, server source-available or proprietary (`D-004`) |

### 1. Personal app (open source) — the base

A desktop app that feels like the Claude app, but you choose the brain.

- Chat with streaming, conversation history, projects (folder of chats + files + instructions), file attachments, local search over your files.
- **Models**: built-in local inference (replacing the Swift Demios Edge with a Rust engine), plus BYOK for OpenAI/Anthropic/Gemini/OpenRouter/any OpenAI-compatible endpoint, plus Ollama/LM Studio if already installed. Model slots per task (chat, fast, embed, vision, STT/TTS) carried over from Demios.
- **Artifacts, Claude-style**: the model emits a self-contained artifact (HTML, React/JSX, SVG, Mermaid, Markdown, code); it renders in a sandboxed webview next to the chat; every edit is a new version; the model can update an artifact in place; export/save to disk. Demios's designers become *artifact types* over time, not separate apps.
- **Tools**: MCP client (stdio + Streamable HTTP) as the one extension mechanism, with per-tool approval and a local side-effect log (the Demios capability contract, simplified).
- **Skills**: Demios-style declarative skills (YAML + prompt + optional tool calls), runnable without Python.
- Local-first storage (SQLite + files), no account required, telemetry off by default.

### 2. Executive assistant (premium) — the Twin, rebuilt around autonomy

Same base app plus a Twin module that is the reason to pay.

- Connects mail, calendar, tasks, messaging (via connectors/MCP).
- Daily brief, triage, drafts replies, schedules, follows up, remembers people and commitments.
- **Autonomy ladder** per action type: suggest → draft → act-with-approval → act-and-report. Every external write goes through the approval queue and side-effect ledger (the Demios rule "the Twin is the only path that sends messages for the user" stays).
- Long-term memory (people, preferences, commitments) stored locally, encrypted.
- Background agent that runs while the app is open (and later as a small daemon), with an approvals surface on mobile via an optional relay.
- Monetisation: licence key / subscription unlocks the Twin module (`D-005`).

### 3. Team version

The open-source desktop app plus a central server.

- Team server: accounts, orgs, roles, SSO (later), shared projects/artifacts/skills/agents, shared knowledge base, presence and sync.
- Desktop stays local-first; the server syncs selected spaces (CRDT-based) and relays.
- Optional shared model endpoints: the team can point everyone at a shared inference box or shared BYOK keys held server-side (keys never sent to clients).
- Self-hostable as one binary + Postgres (or SQLite for small teams); hosted option later.

## Free vs premium

| Feature | Personal (free, OSS) | Twin (premium) | Team |
|---|---|---|---|
| Chat, projects, history | ✓ | ✓ | ✓ |
| Local models + BYOK | ✓ | ✓ | ✓ (+ shared endpoints) |
| Artifacts (render, version, export) | ✓ | ✓ | ✓ (+ shared, comments) |
| MCP tools, skills | ✓ | ✓ | ✓ (+ team catalogue) |
| Local RAG over files | ✓ | ✓ | ✓ (+ team knowledge) |
| Twin: memory, briefs, triage, drafts, autonomous actions | – | ✓ | per-seat add-on |
| Mobile approvals relay | – | ✓ | ✓ |
| Accounts, orgs, roles, SSO, audit | – | – | ✓ |

Principle: anything that only touches the user's own machine and own models is free and open. Paid value is (a) the Twin's autonomy and (b) coordination across people.

## Licence options (`D-004`)

| Option | Client | Pros | Cons |
|---|---|---|---|
| **A. Apache-2.0** client, proprietary Twin + team server | Apache-2.0 | Max adoption, patent grant, easy for contributors and companies | Anyone can fork and close it |
| B. MIT client | MIT | Simplest | No patent grant |
| C. AGPL-3.0 client + team server, commercial licence for others | AGPL | Forks must stay open; protects hosted team server | Scares some companies and contributors |
| D. Apache-2.0 client, team server under FSL/BSL (source-available, converts to Apache after 2 years) | Apache + FSL | Open client, protects server business, still inspectable | "Not OSI open source" for the server |

Recommendation: **D** — Apache-2.0 for the desktop app and shared crates; Twin module proprietary; team server FSL-1.1-Apache-2.0. Inference engine choice must be licence-compatible (llama.cpp MIT, mistral.rs MIT, candle Apache/MIT are all fine).

## Non-goals for the first release

Flows designer, seven no-code designers as separate apps, billing/wallet, mobile app, admin panel, 77 connectors, mail/calendar UI, enterprise appliance. They are either re-expressed as artifacts/MCP tools later or left behind.
