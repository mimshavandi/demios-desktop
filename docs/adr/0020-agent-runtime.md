# ADR-0020: One agent runtime: harness, context and memory for every surface

**Status:** Proposed · **Date:** 2026-11-27 · **Deciders:** project owner, lead · **Extends:** [ADR-0001](0001-repo-and-crate-layout.md), [ADR-0009](0009-security-baseline.md), [ADR-0013](0013-plugin-api.md) (§5, §9), [ADR-0014](0014-demios-node.md) · **Issue:** to be filed on acceptance

## Context

Demios should answer as well as Claude and ChatGPT, mostly on local 4–32B models, with an optional BYOK frontier model. What makes those apps good is not only the model. It's the layer around it: how context is packed, how long chats are handled, what they remember and how, which model answers which request, and how all of that is measured.

Today every surface does this on its own, or not at all. Read from `main` @6bb7335:

- **One real model path.** The chat turn (`apps/desktop/src/chat.rs:150`) runs on `dn-agent`'s loop. Everything else either doesn't call a model yet (titles are the first 48 characters, there are no summaries, no MCP sampling, the Node calls no models, add-on `run_model` is refused) or calls it directly (`dn run`, the tool probe, benchmarks).
- **No context management.** `history()` (`chat.rs:300`) sends one system message plus *every* stored message. Nothing counts tokens for remote models; `context_window` is filled for two providers and read by nobody; local models fail with "too long for the model's context window".
- **No caching.** No provider prompt caching, no llama.cpp slot or prefix reuse.
- **No memory.** The add-on API has `MemoryItem` but no table behind it; chat has no memory; `user_name` in the prompt is hardcoded to "the user".
- **Duplicated pieces.** System-prompt text is assembled in four places; "untrusted" marking exists for knowledge excerpts (`dn-knowledge/src/context.rs`) and as an unrendered type in the plugin API (`Quoted`), and MCP tool output isn't wrapped at all; truncation is by characters in four places; model choice happens in five places; reasoning is switched per caller tag.
- **No measurement.** Usage is summed per turn and dropped; nothing is traced; there's no eval suite beyond benchmarks of raw model speed.

If the Twin, the Node and every later add-on each solved these again, we'd have several weaker copies. This ADR makes them one runtime.

What the field does (research summarised in §9): one context assembler with fixed slots and a policy per surface; compaction instead of silent truncation; two memory layers (an explicit, editable list plus past chats searched as a tool); memory scoped per project with an incognito mode; full-context project knowledge that switches to retrieval when it no longer fits; a per-request router between fast, thinking and frontier models; tool search instead of every tool in every prompt; skills loaded progressively; byte-stable prompt prefixes for caching; and eval suites tied to every prompt and model change.

## Decision

### 1. One runtime, used by every surface

A new crate **`dn-runtime`** (amends ADR-0001) owns the turn: it takes a *request* from a surface and returns a typed *run result*. Surfaces are: chat, project chat, chat skills, agents, add-on handlers and jobs (ADR-0013), the Node (ADR-0014), the CLI (`dn chat`, a new interactive mode on the same runtime), and later the phone. Each surface supplies a **policy** (which slots, budgets, tools, memory scopes and model route it wants); none builds prompts itself.

`dn-agent` stays the loop (steps, tools, cancellation, streaming); `dn-runtime` wraps it with the layers below. `dn-tools` stays the approval gate; `dn-jobs` stays the scheduler and held queue; `dn-knowledge` becomes the retrieval backend.

### 2. Context assembler

Every model call goes through one assembler:

1. **Budget from the real model.** Tokens are counted with the model's own tokenizer (llama.cpp locally; the provider's count or a calibrated estimate remotely). The usable input budget is the model's *effective* context, not its advertised window: by default 60% of `n_ctx` for local models, a per-model setting measured by the eval suite (§7), and the provider's window minus output reserve for BYOK.
2. **Fixed slot order**, the same on every surface: system → tool definitions → skills index → core profile (memory) → project or add-on knowledge → retrieved items → conversation (summary + recent turns) → the new input → a one-line task recap. A surface's policy sets each slot's cap and priority; when over budget the assembler compresses or drops the lowest priority first and records what it dropped.
3. **Byte-stable prefix.** Slots up to the core profile change rarely and are rendered deterministically (no timestamps, sorted keys). The date and other volatile values go at the end. This is what makes caching work (§6).
4. **Untrusted content in one format.** Everything not written by the user or Demios (files, retrieved text, web and MCP tool output, mail and calendar for add-ons) is rendered by one function: delimited, datamarked, neutralised, with its source id. `dn-knowledge`'s `<<<SOURCE` format and the plugin API's `Quoted` become this one renderer; MCP tool results get it too.
5. **Tool results at the source.** Tools return a compact projection with a "more: ref" pointer; old tool results are replaced by placeholders before older messages are summarised; the last error stays.

### 3. Long conversations: compaction, never silent loss

When a conversation reaches 70% of its budget, the runtime summarises the oldest part into a running summary (kept with the conversation, synced like messages) and keeps the recent turns verbatim. The full transcript stays stored and searchable (§4.2), so nothing is lost; the UI marks where earlier messages were summarised. Jobs and add-on handlers start from a fresh context every run.

### 4. Memory: one store, two layers, scoped

**4.1 Memory items** (the explicit layer). One host-owned table, used by chat and add-ons alike (implements ADR-0013 §5 and extends its item):

| Field | Meaning |
|---|---|
| `scope` | account, project, or add-on (an add-on sees only its own and what the user grants) |
| `type` | preference, fact, person, commitment, project, style, correction |
| `subject` | the person, project or topic it's about |
| `text` | the memory, short |
| `valid_from`, `valid_to` | when it was true; superseded items keep their history |
| `status` | pending, active, superseded, retracted |
| `trust` | user-stated > user-confirmed > from the user's own content > from third-party content |
| `sources` | the messages, files or items it came from |
| `created_by` | the user, or which model and prompt version |

- **Writes.** The user says "remember …" or edits the list (top trust, immediate). Otherwise a background pass extracts candidates *add-only* with constrained output; a separate pass marks older items superseded. Candidates from third-party content (a received mail, a web page, a file someone else wrote) can only describe their source ("Alice says the review moved to Friday") and never become preferences or instructions. Every inferred candidate waits as **pending** until the user confirms it, unless the user turns on "remember automatically" for trusted scopes. Never: health, finances, identity numbers, passwords.
- **Reads.** A short **core profile** (500–1,500 tokens) is rebuilt from active, high-trust items in scope and sits in the cached prefix. Further items are retrieved per request (§5) and shown as "used memories" chips the user can open to see why.
- **Control.** A Memory page: list, filter, edit, delete, see sources, the pending queue, pause memory, export. **Incognito chats** neither read nor write memory and aren't indexed. Each project can be isolated from account memory. Deleting a memory or its sources cascades (ADR-0013 §5 purge, #280) and syncs as a tombstone.

**4.2 Past conversations** (the implicit layer). Not injected; searchable through a `search_chats` tool the model can call, over the same index as §5, scoped like memory and excluding incognito.

### 5. Retrieval

One retrieval service for project files, past chats, memory items and add-on data: hybrid BM25 (SQLite FTS5) plus embeddings (the "Project search" model, nomic v2 by default) plus entity and recency filters, fused with reciprocal rank fusion; an optional reranker later. Chunks get a short context header (title, author, date, section); on a Node or while idle, a model writes a one-line context note per chunk before embedding ("contextual retrieval"). Results go in as dated, source-attributed items, top 3–5 by default.

**Project knowledge** goes into context whole while it fits the policy's budget and switches automatically to retrieval through a `search_project` tool when it doesn't, and back when it shrinks.

### 6. Models, routing and speed

- **Slots become roles:** `chat`, `fast` (already declared, unused), `thinking`, `embed`, and optionally `frontier` (a BYOK model). A surface asks for a role, not a model.
- **Per-request router.** A cheap classifier (rules first, a small local model later) picks fast, thinking or frontier from the request: complexity, tool needs, length, explicit intent ("think hard"). The user's explicit model choice always wins. If a role isn't configured, the request fails with a clear message; there is no silent fallback to another model or provider.
- **Effort and thinking are separate controls.** Thinking is off by default on local models unless the router or the user turns it on.
- **Caching.** Provider prompt caching on the stable prefix; for local models, `cache_prompt` with one llama.cpp slot per surface or project, warm slots saved and restored across restarts on the Node.
- **Speed.** Speculative decoding for 14–32B local models where the build supports it; every step streams, including tool status.

### 7. Tools and skills at scale

- **Tool search.** Only a few always-on tools plus a `search_tools` tool are sent; other MCP and Connect tools are loaded when found. Small models get 1–5 tool schemas per step.
- **Skills** move to the open Agent Skills format (SKILL.md, progressive disclosure: name and description always, body and files when used). The unused agent-manifest skills in `skills/agents/assistant.default/agent.yaml` are folded in or removed.
- **Approvals** stay as they are (ADR-0013 §9, tiers, held calls, one-time tokens); tools without a read-only hint count as writes.

### 8. Runs, tracing and evals

- **Typed run result** for every call: why it stopped, model, tokens in and out, cache hits, wall time, cost, steps, and what the assembler dropped. Usage is stored (fixing #313 for jobs and adding it for chat).
- **Durable runs** for jobs and paused approvals: an append-only step log, resumable after a restart; side-effecting tools keep idempotency keys.
- **Tracing**: structured spans per model and tool call (the `tracing` crate replaces ad-hoc `eprintln!`), kept locally and never sent anywhere. Traces are replayable in evals.
- **Eval suite** (`dn eval`), run on every prompt, model or runtime change:
  - **parity:** 300–500 golden prompts across chat, projects, tools and research, with a private holdout; pairwise against reference answers from Claude and ChatGPT, judged by a fixed model from another family, order swapped, length controlled, calibrated on ~100 human-labelled pairs;
  - **memory:** LongMemEval through our own memory pipeline (reporting the drop against reading the history directly) plus our own set for deletion, project isolation, incognito and poisoning;
  - **retrieval:** recall@k, groundedness, citation precision;
  - **tools:** τ²-bench-style tasks reported as pass^k, plus our approval and injection cases;
  - **speed:** time to first token, tokens per second, p50/p95 per hardware tier, cold and warm cache;
  - **router:** agreement with an oracle, escalation rate, cost per win.
- **System prompts are versioned files** with a changelog, each version tied to its eval run.

### 9. Research behind this

Agent harnesses (Claude Agent SDK, OpenAI Agents SDK, LangGraph, OpenHands, Letta, Goose, smolagents, Microsoft Agent Framework, OpenClaw, Hermes Agent), products (Claude, ChatGPT, Gemini, AnythingLLM, Open WebUI), memory systems (Letta, mem0, Zep/Graphiti, LangMem, A-MEM, Cognee, MemoryOS/MemOS), and studies on long context (NoLiMa, RULER, Chroma "context rot", lost in the middle), retrieval (Anthropic contextual retrieval), structured output, prompt-injection defences (spotlighting, CaMeL) and memory poisoning (SpAIware, MINJA, AgentPoison). Sources are listed in the study (demios-pro `docs/twin/harness-context-memory.md` and its follow-up). Many published benchmark numbers are vendor-reported and not comparable; that's why §8 builds our own.

## Options considered

| Option | Verdict |
|---|---|
| **A. One runtime crate used by every surface (this ADR)** | Chosen: one fix improves chat, projects, the Node and every add-on; one place to measure |
| B. Shared helpers, each surface assembles its own prompts | Rejected: the duplication we have now, with drift |
| C. Adopt an external framework (LangGraph, Letta, mem0) | Rejected: Python or a server, not local-first Rust; memory and approvals would leave our consent model; we take their designs, not their code |
| D. Memory as a graph database (Graphiti-style) | Rejected for now: several model calls per write is too heavy for local models; we take its validity model only |
| E. Rely on long context windows instead of memory and retrieval | Rejected: models degrade well before their window, local prefill is slow, and context costs money on BYOK |

## Consequences

- Chat gets compaction, memory, project-knowledge switching, routing, caching and tool search; add-ons and the Node get the same through ADR-0013's `run_model`, memory and knowledge calls, which can finally be offered.
- `history()` and the four prompt builders are replaced; the knowledge context format and `Quoted` merge into one untrusted renderer.
- New storage: memory items, conversation summaries, run results and usage, traces, retrieval index over chats. Memory and summaries sync (dn-sync gains collections); traces and the index stay on each device.
- New privacy-policy lines for memory, summaries and incognito, for the owner's approval.
- More background work on idle or on the Node (memory extraction, chunk notes, cache warming), within the daily budget.
- Temporary choices on acceptance, each an issue: effective-context defaults until measured; rules-only router at first; no reranker at first; no graph store; tool search for MCP before Connect.

## Rollout

Each stage ships behind a setting and is measured by §8 before it's turned on by default.

| Stage | Contents |
|---|---|
| **R0 Measure** | `dn eval` skeleton: golden prompts, LongMemEval runner, speed suite; typed run result; usage stored; tracing |
| **R1 Runtime and context** | `dn-runtime` crate; assembler with tokenizer budgets and slots; one untrusted renderer; compaction; chat and `dn chat` moved onto it |
| **R2 Speed** | stable prefixes, provider caching, llama.cpp slot reuse and save/restore, speculative decoding where supported |
| **R3 Memory** | memory items table and API (ADR-0013 §5), Memory page and chips (design first), pending queue, incognito, `search_chats`, sync of memory and summaries |
| **R4 Retrieval and tools** | hybrid retrieval service, contextual chunk notes, project full-context ↔ retrieval switch, tool search, Agent Skills format |
| **R5 Routing** | roles, per-request router, effort/thinking controls, frontier role for research and long agentic chains |
| **R6 Add-ons and Node** | `run_model`, memory and knowledge offered to add-ons on desktop and Node; durable job runs |

R0 goes first so every later change is measured; R1–R2 give the biggest quality and speed gains for chat; R3–R6 follow. The Twin's plan (demios-pro) builds on R1, R3 and R6 instead of its own copies.

## Open questions for the owner

- **Q1** Approve one runtime crate (`dn-runtime`) and this rollout order.
- **Q2** Memory defaults: inferred memories wait for confirmation (recommended) or are kept automatically with an undo.
- **Q3** Incognito chats: offered from the start (recommended).
- **Q4** Routing: the router may pick the frontier role on its own when one is configured (recommended), or only when the user asks.
- **Q5** Parity references: we generate reference answers from Claude and ChatGPT for the private eval set through their APIs, at our own cost, under their terms (recommended), or judge without references.
