# ADR-0004: Claude-style artifacts runtime

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner

## Context
Demios artifacts are proprietary JSON (PenDocument, Excalidraw elements) rendered by bespoke editors, keep only the latest snapshot, and have no artifact identity across messages. The user wants them to behave like Claude artifacts: the model writes a self-contained thing, it renders next to the chat, it is versioned, and the model can edit it.

## Decision
- **Artifact** = `{id, conversation_id, type, title, versions[]}`; **version** = `{n, content, created_by: model|user, parent_n, message_id, created_at}`. Stored in SQLite; content as text blob. Append-only: every change is a new version; rollback = new version copying an old one.
- **Types (v1):** `html`, `react` (single-file JSX, bundled in-app with a pinned esbuild-wasm/sucrase + import map of an allow-listed CDN set or vendored libs), `svg`, `mermaid`, `markdown`, `code` (with language). Later: `slides`, `sheet`, `board`, `form` — Demios designers return as artifact types with templates.
- **Model protocol:** the model calls tools `artifact_create {type,title,content}`, `artifact_update {id, old_str, new_str}` (exact-match patch, like Claude), `artifact_rewrite {id, content}`. Tools are exposed to every model; for models without tool calling, a fenced-block fallback (`<artifact id= type= title=>…</artifact>`) is parsed from the stream.
- **Rendering:** a separate webview/iframe on a unique custom origin (`artifact://<id>`), `sandbox="allow-scripts"` without `allow-same-origin`, strict CSP (no network by default; per-artifact "allow network" toggle), no Tauri IPC exposed. Communication via `postMessage` only (size, errors, "ask the model to fix" button that sends console errors back).
- **Export:** save to file, copy, open in browser. Team product: share an artifact to a space.
- **Inline blocks** (Demios A2UI: chart, kpi, table, status, action) stay as lightweight in-message blocks — not artifacts.

## Options considered

| Option | Fidelity to Claude | Security | Effort |
|---|---|---|---|
| **A. Text artifacts + sandboxed webview + patch tools** | High | Good (origin isolation, CSP) | Medium |
| B. Port PenDocument/Excalidraw editors | Low | Good | High, proprietary formats |
| C. Render in main webview | High | **Bad** — artifact JS can reach app IPC | Low |

## Consequences
- Easier: any model that writes HTML can produce artifacts; versions make "undo the model's change" trivial.
- Harder: React artifacts need an in-app bundler and a vetted library set; small local models struggle with exact-match patches → fall back to rewrite.
- Revisit: the Demios design-QA loop (validate → critique) as an optional "polish" skill.

## Action items
T-ART-01 store + versions, T-ART-02 sandbox renderer per OS, T-ART-03 tool protocol + fenced fallback parser, T-ART-04 React bundling, T-ART-05 security review of sandbox (escape tests).
