# ADR-0002: Desktop shell — Tauri 2

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner

## Context
"Everything in Rust", desktop on macOS, Windows and Linux, a Claude-style chat UI, and **artifacts that render arbitrary model-generated HTML/React in a sandbox**. Rendering HTML is the hard requirement: a pure-Rust GUI cannot render web artifacts without embedding a webview anyway.

## Decision
**Tauri 2** with a Rust backend (all logic in crates) and a thin web UI. UI framework: **Leptos** (Rust → WASM) if the maintainers choose "Rust all the way", otherwise **SolidJS/React + TypeScript (strict)**. Artifacts render in a separate, locked-down webview / sandboxed iframe with its own origin and CSP (ADR-0004).

## Options considered

| Option | Rust share | Renders HTML artifacts | Maturity | Bundle size | UI dev speed |
|---|---|---|---|---|---|
| **A. Tauri 2 + Leptos** | ~100% | Yes (system webview) | Tauri high, Leptos medium | ~10–20 MB | Medium |
| **B. Tauri 2 + TS UI (Solid/React)** | Backend 100%, UI TS | Yes | High | ~10–20 MB | High (can lift Demios React chat components) |
| C. Dioxus desktop | ~100% | Yes (webview) | Medium | Similar | Medium |
| D. egui / iced / Slint (native) | 100% | **No** — needs a webview bolted on | Medium–High | Small | Low for rich chat UI |
| E. Electron | 0% backend | Yes | High | 150 MB+ | High — contradicts "Rust" |

## Trade-offs
- System webviews differ (WebKit on macOS/Linux, WebView2 on Windows); artifacts must be tested on all three — acceptable, same as Claude desktop's constraints.
- Leptos keeps one language and lets UI and core share `dn-core` types directly; TS is faster to build and hire for, and Demios's chat/A2UI React components could be reused. Recommendation: **B with strict TypeScript** for speed, with `dn-core` types exported to TS via `specta`/`ts-rs` so contracts stay Rust-owned. Choose A if "Rust everywhere" outweighs UI velocity.

## Consequences
- Easier: one codebase for 3 OSes, small installers, built-in updater and code signing hooks, capability-scoped IPC.
- Harder: webview quirks; Linux WebKitGTK performance.
- Revisit: mobile (Tauri 2 supports iOS/Android) for a companion app.

## Action items
- T-SHELL-01 shell skeleton with chat screen and streaming; T-SHELL-02 settings/models screen.
