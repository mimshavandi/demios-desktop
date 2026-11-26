# Design standard — demios-desktop

Status: **accepted** (2026-11-24) (T-DES-01, ADR-0010). Visual preview: `docs/design/preview.html`. Applies to `apps/desktop/ui` and the chrome of `ui/artifact-sandbox`. Tokens: `ui/design-tokens`. Built from the `design:design-system` and `ui-ux-behaves` guidance: tokens → components with defined states → patterns, WCAG 2.2 AA, and every container covering `loading · empty · ready · error` (+ `warning` where there is risk).

## 1. Principles

1. **Content first, chrome quiet.** The conversation and the artifact are the product. UI chrome is neutral grey; colour is reserved for meaning (accent = primary action/selection, mint = on-device, red/amber/green = status).
2. **Native on every OS.** System fonts, OS title-bar conventions (traffic lights on macOS, caption buttons on Windows), platform shortcuts (`⌘` / `Ctrl`). It should feel like a good native app, not a website.
3. **Show where the model runs.** Every answer, model picker and tool call shows its source: *On this device* (mint) or the provider's name. Privacy is visible, never implied.
4. **The user stays in control.** Anything that writes outside the app asks first, says exactly what will happen, and is logged. Prefer undo over "Are you sure?".
5. **One way to do a thing.** One component per job, one name per concept. If the heading says it, the body doesn't repeat it.
6. **Accessible by default.** Keyboard for everything, visible focus, screen-reader names, never colour alone, usable at 200 % zoom.

## 2. Foundations (tokens)

All values live in `ui/design-tokens/tokens.json`; components use CSS variables `--dn-*` only.

| Group | Tokens | Rule |
|---|---|---|
| Colour roles | `bg, surface, surface-sunken, surface-raised, sidebar, border, border-strong, text, text-muted, text-subtle, text-on-danger, accent, accent-hover, accent-soft, accent-text, local, local-soft, danger(-soft), warning(-soft), success(-soft), focus, code-bg, overlay` | Roles, not hues. Each role has a light and dark value; contrast verified by script (body text ≥ 7:1, secondary ≥ 4.5:1, focus ring ≥ 3:1). |
| Brand | accent `#5658DD` (Demios indigo), local `#24E5AF` family (Demios mint, darkened for light-mode text) | Carried from the Demios palette for continuity. |
| Type | `--dn-font-sans` (SF Pro / Segoe UI Variable / system), `--dn-font-mono`, optional `--dn-font-serif` for long-form reading mode | Scale `xs 12 · sm 13 · base 14 · md 15 · lg 17 · xl 20 · 2xl 24`. Messages use `md` (15/24) for reading comfort; UI uses `base`. Weights 400/500/600 only. |
| Space | 4-px grid: `1=4 … 16=64` | Proximity groups: 4–8 inside a control, 12–16 between related items, 24–32 between sections. |
| Radius | `sm 4 · md 6 · lg 10 · xl 14 · full` | Controls `md`, cards/panels `lg`, dialogs `xl`, avatars/pills `full`. |
| Elevation | `shadow-1..3` | 1 = cards, 2 = popovers/menus, 3 = dialogs. Dark mode relies on surface steps more than shadow. |
| Motion | `fast 120 · base 180 · slow 260`, one easing | Only for state change (open/close, stream cursor). All zero under `prefers-reduced-motion`. |
| Layout | sidebar 264, chat column max 720, artifact pane min 420, titlebar 40, hit target ≥ 28 (pointer) | |

## 3. App layout

```
┌───────────── titlebar (40): window controls · conversation title · model badge ─────────────┐
│ sidebar (264)        │ chat column (≤720, centred)              │ artifact pane (≥420, resizable) │
│ + New chat (⌘N)      │ messages                                  │ title · type · version v3 ▾     │
│ Search (⌘K)          │  user: right-aligned bubble, surface-sunk │ [Preview | Code] tabs           │
│ Projects             │  assistant: full-width text, no bubble    │ sandboxed render                │
│ Recent chats         │  tool call cards, approval cards          │ error bar + "Ask to fix"        │
│ ───────────          │                                           │ Copy · Download · Open in       │
│ Models · Settings    │ composer (sticky bottom, ≥52)             │   browser                       │
└──────────────────────┴───────────────────────────────────────────┴─────────────────────────────────┘
```

- **Stable frame:** titlebar and sidebar never change between screens; settings open as a full page in the main area, not a modal.
- Sidebar collapsible (`⌘\`). Artifact pane opens when an artifact is created or selected, closes with `Esc` when focus is inside it, and remembers its width.
- **Responsive:** ≥1280 all three panes; 1024–1279 sidebar collapses to icons when the artifact pane is open; <1024 artifact pane becomes a full-width overlay with a back button. Never horizontal scroll. Strings can grow 30 % (translation).

## 4. Components

Headless primitives + our tokens; **no styled component library** (no MUI/Chakra/Ant). Primitives: **Kobalte** if the UI is SolidJS, **Radix Primitives** if React (W-shell picks the framework in T-SHELL-01). Icons: **Lucide**, 16 px in UI, 1.5 stroke. Each component below must implement every listed state before it is used on a screen.

| Component | Variants / sizes | Required states | Notes |
|---|---|---|---|
| Button | primary, secondary, ghost, danger; sm 28 / md 32 | default, hover, active, focus-visible, disabled, loading | Verb + noun labels ("Download model"). One primary per view. Never disable a submit to signal errors — validate and explain instead. |
| IconButton | ghost; sm/md | + tooltip on hover/focus | Always an `aria-label`; tooltip shows label + shortcut. |
| TextField / TextArea | md | default, focus, invalid (+message), disabled, read-only | Label above, help below; error text replaces help, linked by `aria-describedby`. |
| Composer | — | empty, typing, sending, streaming (Stop), disabled (no model → shows "Connect a model") | `Enter` send, `Shift+Enter` newline, `⌘↑` edit last, attachments chip row, model + skill chips. |
| Select / Combobox | md | + empty results, loading options | Model picker is a Combobox grouped *On this device* / provider. |
| Switch | — | on, off, disabled | Only for settings that apply **immediately**; never inside a form with a Save button (use Checkbox). |
| Checkbox, Radio | — | checked, unchecked, indeterminate (checkbox), disabled, invalid | |
| Tabs | — | selected, focus, disabled | Arrow-key navigation; state in URL/route for settings tabs. |
| Menu / ContextMenu | — | item hover, disabled item, destructive item | Shortcut hints right-aligned. |
| Dialog | default, alert (destructive) | opening, open, closing | Focus trap, `Esc` closes, focus returns to the trigger. Only for decisions that block; prefer inline. |
| Toast | info, success, warning, error | entering, visible, exiting | `role=status` (errors `role=alert`); auto-dismiss 5 s except errors; undo action where possible. Max 3 stacked. |
| Tooltip | — | — | Never the only place for essential info. |
| Badge | neutral, accent, local, danger, warning, success | — | Text + icon, never colour alone. |
| Skeleton | line, block | — | For known layouts (history list, model list). Spinners only inside buttons or for < 1 s indeterminate. |
| EmptyState | — | — | Title, one sentence, one primary action. |
| InlineMessage | info, warning, error, success | — | In-place feedback next to what caused it. |

### Chat-specific

| Component | States / rules |
|---|---|
| **Message (assistant)** | streaming (caret at end, `aria-busy`), complete, stopped by user ("Stopped" note + Continue), error (inline error + Retry, prompt kept). Footer on hover/focus: Copy · Retry · model badge · tokens/s (local). Markdown with code blocks (language label, Copy). |
| **Message (user)** | sent, editing (inline, `Esc` cancels), failed to send (Retry, text preserved). |
| **Thinking block** | collapsed by default with duration ("Thought for 12 s"); expandable; never mixed into the answer text. |
| **Tool call card** | queued, running (live elapsed), succeeded (collapsed result, expandable), failed (`CapabilityError.user_message` + Retry if retryable / Reconnect if `reconnect_required`). Shows server + tool name + risk badge. |
| **Approval card** (inline, not a modal) | Title says the action in plain words ("Write to `notes/todo.md`"), shows arguments (diff for file writes), risk badge, buttons **Allow once** (primary), **Always allow for this chat** (secondary, hidden for destructive/sensitive), **Deny** (ghost). Keyboard: `⌘↵` allow, `Esc` deny. Never auto-expire into "allow". |
| **Artifact card** (in message) | title, type icon, version, "Open" — clicking focuses the artifact pane. |
| **Artifact pane chrome** | loading (render skeleton), ready, runtime error (error bar with first error + "Ask to fix"), blocked network (inline notice + per-artifact toggle). Version switcher: `v3 ▾` list with author (model/you) and time; restoring creates a new version (shown as "v4 — restored from v2"). |
| **Model badge** | `On this device · Qwen3 8B` (local tint) or `Anthropic · Claude Sonnet` (neutral). Fit status on model rows: Ready / Caution / Memory pressure / Won't fit, each with a one-line reason. |
| **Download row** | queued, downloading (progress bar with size + ETA, Pause), paused, verifying, ready, failed (reason + Retry). |

## 5. Behaves (states every container must cover)

Keys follow `context.state` from ui-ux-behaves; each maps to a component above.

| Container | loading | empty | ready | error | warning |
|---|---|---|---|---|---|
| Chat list (sidebar) | Skeleton rows | EmptyState "No chats yet" + New chat | list | InlineMessage + Retry | – |
| Conversation | Skeleton messages | Welcome: model status + 3 example prompts | messages | per-message error, prompt kept | "No model connected" banner → Connect |
| Model list | Skeleton | EmptyState "Download a model or add a key" | rows with fit status | InlineMessage per row | Fit = Caution / Memory pressure |
| Artifact pane | render skeleton | – (pane closed) | render | error bar + Ask to fix | network blocked notice |
| MCP servers | Skeleton | EmptyState + Add server | rows with status | Reconnect | risky permissions notice |
| Settings forms | – | – | form | field errors + summary at top on save | unsaved changes guard on leave |

Destructive actions (delete chat, delete model, remove server): act immediately, show a toast with **Undo** (soft delete for 10 s / trash). A confirmation dialog only when undo is impossible (e.g. deleting a downloaded model file > 1 GB), and it names the object and size.

## 6. Writing

Sentence case; no full stop on labels/buttons/titles; verb + noun buttons ("Add key", never "OK"/"Submit"); user words not system words ("Model", not "inference backend"; "On this device", not "local runtime"); errors say what happened, why if known, and what to do ("Couldn't reach Anthropic. Check your connection, then retry."). Never blame the user. Numbers with units ("4.7 GB", "38 tokens/s").

## 7. Accessibility and keyboard

WCAG 2.2 AA. Every control has name/role/state; streaming answers announced politely when complete (not per token); focus moves deliberately (new chat → composer; dialog close → trigger; approval resolved → next item). Axe in E2E must report nothing serious/critical; keyboard walkthrough of the MVP flow before release. Global shortcuts: `⌘N` new chat, `⌘K` search/command palette, `⌘\` sidebar, `⌘,` settings, `⌘.` stop generation, `⌘⇧C` copy last answer. Windows/Linux use `Ctrl`.

## 8. Enforcement

- **Design reference:** maintainers keep a browsable copy of this standard (tokens, components with previews of every state, screenshots) as a shared design artifact. A new screen or component is designed and reviewed there before it's built; this file and `ui/design-tokens` stay the source of truth, and a PR that changes either updates the reference in the same change.

- `ui/design-tokens`: `node scripts/build.mjs --check` in CI (stale files or contrast failure fails the build).
- UI lint (T-DES-02, after W-shell's UI lands): stylelint rule banning raw hex/rgb, px font sizes and non-token spacing in `apps/desktop/ui` and `ui/artifact-sandbox`; `/ui-ux-behaves:audit` ratchet for known anti-patterns (disabled submit, spinner over area, "Are you sure?").
- Review: each screen PR includes light + dark screenshots and a note of which behaves it covers; the lead runs `/ui-ux-behaves:review` on the MVP screens.
