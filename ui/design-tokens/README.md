# @demios/design-tokens

The one source of visual values for demios-desktop (ADR-0010, `docs/design/design-system.md`).

- Edit **`tokens.json`** only, then run `node scripts/build.mjs` to regenerate `tokens.css` and `tokens.ts`.
- `node scripts/build.mjs --check` fails if the generated files are stale or any declared text/background pair falls below its WCAG contrast minimum (both themes). CI runs it.
- Use roles, not values: `var(--dn-color-text-muted)`, `var(--dn-space-4)`. Raw hex, px font sizes or ad-hoc spacing in components fail review.
- Themes: light by default, dark via `prefers-color-scheme`, forced with `data-theme="light|dark"` on `<html>`.

## Lint: no raw values in UI code

`scripts/lint-ui.mjs` (zero dependencies, run in CI) checks `apps/desktop/ui/src` and `ui/artifact-sandbox/src`, in CSS files and in CSS inside TS/TSX strings, for:

| Rule | Instead use |
|---|---|
| `raw-color`: hex, `rgb()`, `hsl()` | `--dn-color-*` |
| `px-font`: px in `font` or `font-size` | `--dn-text-*` |
| `px-spacing`: px over 1 in margin, padding, gap or inset (±1px hairlines are fine) | `--dn-space-*` |
| `px-radius` | `--dn-radius-*` |
| `raw-shadow` | `--dn-shadow-*` |
| `raw-z-index` | `--dn-z-*` |

It is a ratchet. `lint-baseline.json` holds today's count per file and rule, and CI fails when a count grows. When you remove raw values, run `node ui/design-tokens/scripts/lint-ui.mjs --update` and commit the lower baseline (CI asks for that too). `--list` prints every finding.

Today's baseline is only the artifact sandbox's frame CSS (`document.ts`): the frame doesn't load the app's tokens yet.

