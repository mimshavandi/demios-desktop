# ADR-0005: Agents and skills — manifests + model-driven tool-use loop

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner

## Context
Demios's ADL → skill.yaml + handler → versioned prompt format is good and data-driven, but has no schema, a hand-written YAML parser, Python handlers, and a 5.7k-line hard-coded orchestrator. Modern models route themselves via tool calls.

## Decision
- One **agent loop** in `dn-agent`: system prompt + tools (skills, MCP tools, artifact tools) → model → tool calls → results → repeat, with streaming, cancellation, step limits and token accounting. No central intent classifier.
- **Manifests** keep Demios's shape, formalised with JSON Schema and parsed with serde:
  - `agent.yaml`: id, version, description, prompt ref, model slots, allowed skills/tools, policies.
  - `skill.yaml`: id (dotted), description, input/output JSON Schema, `kind: prompt | tool_chain | builtin`, risk tier, prompt ref.
  - `prompts/<id>/manifest.yaml` + `v1.md` with `active_version` (unchanged from Demios).
- Skills of kind `prompt` and `tool_chain` need no code → portable and user-authorable. `builtin` skills are Rust trait objects registered by dotted id (the `register_handler` idea).
- **Model slots** per agent (`primary`, `fast`, `embed`, `vision`, `stt`, `tts`) resolved by the ported resolver; missing source → typed `ModelSourceRequired { capability, agent, slot, recovery_actions }` that the UI turns into a "connect a model" prompt (Demios rule: never silently fall back).
- **Caller tag** on every call drives the reasoning on/off policy (port of `_REASONING_PROSE_CALLERS`).
- Compatibility note: we will check alignment with Anthropic's Agent Skills folder format (SKILL.md) so skills are shareable; decision on dual-format support in T-AGT-04.

## Options considered

| Option | Portability | Effort | Fit for small local models |
|---|---|---|---|
| **A. Tool-use loop + declarative skills** | High | Medium | Good with grammar-constrained tool calls |
| B. Port Neumann routing | Low | High | Better for very weak models, but brittle |
| C. Adopt an external agent framework (e.g. rig) | Medium | Low | Unknown; adds dependency (ask before adding) |

## Consequences
Easier: users write skills in YAML/Markdown. Harder: small local models are worse at tool selection → mitigate with GBNF/JSON-schema constrained decoding in llama.cpp and fewer, better-described tools.

## Action items
T-AGT-01 manifest schemas + loader, T-AGT-02 agent loop, T-AGT-03 prompt registry, T-AGT-04 skill format compatibility study, T-PORT-01 port selected Demios llm-skills' prompts.
