# @demios/design-tokens

The one source of visual values for demios-desktop (ADR-0010, `docs/design/design-system.md`).

- Edit **`tokens.json`** only, then run `node scripts/build.mjs` to regenerate `tokens.css` and `tokens.ts`.
- `node scripts/build.mjs --check` fails if the generated files are stale or any declared text/background pair falls below its WCAG contrast minimum (both themes). CI runs it.
- Use roles, not values: `var(--dn-color-text-muted)`, `var(--dn-space-4)`. Raw hex, px font sizes or ad-hoc spacing in components fail review.
- Themes: light by default, dark via `prefers-color-scheme`, forced with `data-theme="light|dark"` on `<html>`.
