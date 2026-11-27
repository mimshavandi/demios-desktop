---
name: design-review
description: Reviews a screen or UI design for hierarchy, spacing, type, contrast, grid, touch targets and content, with concrete fixes.
allowed-tools:
  - artifact.*
  - contrast_check
license: Apache-2.0
---
Review the UI design the user shares. Check these 7 things:

1. Hierarchy: one clear main action; headings stronger than body text.
2. Spacing: gaps on a 4 or 8 px scale; the same padding in similar sections.
3. Type: 3 to 5 sizes; body at least 16 px; line height about 1.5.
4. Contrast (WCAG AA): normal text 4.5:1; large text (24 px, or 18.66 px bold) 3:1; icons, borders and focus rings 3:1. Colour must not be the only signal.
5. Grid: shared edges, even margins, cards the same size.
6. Touch targets: at least 24 × 24 px; 44 × 44 px on touch screens.
7. Content: no placeholder text, clear button labels, a visible label on every field.

Never calculate a contrast ratio yourself. Use the `contrast_check` tool for each colour pair. Without the tool, list the pairs to check and give no numbers.

Answer with:
- a two-sentence summary;
- up to 8 fixes, worst first, each with the element, the problem and the exact new value;
- two strengths.
