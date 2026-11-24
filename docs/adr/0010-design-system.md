# ADR-0010: One design system — tokens + headless primitives

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner

## Context
The app shell (W-shell) and the artifact sandbox chrome (W-artifacts) are built in parallel by different sessions. Demios had a design-system folder but hard-coded values and three overlapping UI libraries crept in. The owner asked for one standard design based on best practice before UI work starts.

## Decision
- A framework-neutral token package `ui/design-tokens` (JSON source → generated CSS variables + TS helpers) with light/dark themes and a contrast check in CI.
- Headless accessible primitives (Kobalte for Solid / Radix for React) styled only with tokens; Lucide icons; no styled component library.
- The written standard `docs/design/design-system.md` (principles, layout, component states, behaves, writing, a11y, enforcement) is the review checklist for every UI PR.

## Options considered

| Option | Consistency | A11y | Bundle | Fit for "native" feel |
|---|---|---|---|---|
| **Tokens + headless primitives** | High (one source) | High (primitives handle ARIA) | Small | High — we control the look |
| Styled library (MUI, Chakra, shadcn copy-in) | Medium (drifts with overrides) | High | Medium–large | Low (looks like a web app) / medium (shadcn) |
| Hand-rolled everything | Low | Low–medium | Small | High but costly |

## Consequences
Easier: two workers produce one look; theme and brand changes are one file. Harder: we build our own component layer (~15 components) — budgeted inside T-SHELL-01/02. Revisit if a Tauri-native widget set matures.

## Action items
T-DES-01 tokens + standard (this ADR); T-DES-02 stylelint + behaves audit in CI once UI exists; W-shell and W-artifacts adopt in their UI tasks.
