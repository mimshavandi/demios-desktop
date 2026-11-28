---
name: design-review
description: Reviews a screen or UI design for hierarchy, spacing, type, contrast, grid, touch targets and content, with concrete fixes.
allowed-tools:
  - artifact.*
  - contrast.check
license: Apache-2.0
---
Review the user interface design the user shares: a screenshot, a mock-up, a description, or its HTML and CSS. Give feedback they can act on, worst problems first.

**The seven categories.** Look at each one:

1. **Hierarchy.** Is it obvious what to read and do first? Are headings clearly stronger than body text, and is there one main action per screen?
2. **Spacing.** Do the gaps follow a scale, such as multiples of 4 or 8 px? Do similar sections have the same padding? Are related things closer to each other than to unrelated things?
3. **Type scale.** Use 3 to 5 text sizes with a clear step between them. Body text should be at least 16 px on the web; never below 12 px. Body line height is about 1.4 to 1.6. Weight should carry meaning, not be bold everywhere. The layout must survive text spacing raised to: line height 1.5, paragraph spacing 2 × the font size, letter spacing 0.12 em, word spacing 0.16 em (WCAG 1.4.12).
4. **Contrast** (WCAG 2.2, level AA unless they ask for AAA):
   - Normal text: at least 4.5:1 against its background.
   - Large text, meaning at least 24 px regular or at least 18.66 px bold (18 pt and 14 pt): at least 3:1.
   - Icons, input borders, focus indicators and other parts needed to use the page: at least 3:1 against what's next to them (WCAG 1.4.11).
   - AAA: 7:1 for normal text, 4.5:1 for large text.
   - Colour is never the only way information is shown, such as an error shown only in red (WCAG 1.4.1).
5. **Grid and alignment.** Do elements share edges and columns? Are margins the same on both sides? Are cards in a row the same size? Does it reflow at 320 px wide without scrolling sideways (WCAG 1.4.10)?
6. **Touch targets.** Every control is at least 24 × 24 px (WCAG 2.5.8, AA). On touch screens, aim for 44 × 44 px. Small targets need enough space around them that they don't overlap their neighbours.
7. **Content.** Is there placeholder text, lorem ipsum or a vague button label ("Click here", "Submit")? Does every field have a visible label? Do error messages say how to fix the problem?

**Contrast is measured, never estimated.** Never work out or guess a contrast ratio yourself.
- If the `contrast_check` tool is available, call it for every text and background pair you comment on, and quote its result.
- If it isn't available, don't state a ratio. List the colour pairs that need checking.
- From a screenshot, colours are your reading of the image. Say that the hex values are approximate.

**Your answer**
1. **Summary:** two sentences on the design's strengths and its main problem.
2. **Fix first:** up to 8 issues, most important first. Each has:
   - its category;
   - what's wrong, with the exact element and values ("the grey caption #9A9A9A on white");
   - the fix, with values ("darken it to #6B6B6B", "raise the padding from 12 to 16 px").
3. **What works:** two or three specific strengths.
4. **Not checked:** anything you couldn't judge from what they shared, such as hover states or the mobile layout.

If they asked a specific question, answer it first. Review; don't redesign. If they want a written report and artifacts are available, put it in a Markdown document artifact.

The WCAG thresholds above are adapted from web-quality-skills by Addy Osmani (MIT licence). See NOTICE.md in this folder.
