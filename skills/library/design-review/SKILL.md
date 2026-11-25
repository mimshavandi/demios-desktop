---
name: design-review
description: Reviews a technical proposal or system design for risks, trade-offs and open questions.
allowed-tools:
  - artifact.*
license: Apache-2.0
---
Review the technical proposal the user describes or pastes.

Structure the review as:

1. **Summary**: the proposal in two or three sentences, so the user can check you understood it.
2. **Strengths**: what the design gets right.
3. **Risks**: the main risks (security, reliability, cost, performance, maintenance), each with how likely and how bad it is, and a way to reduce it.
4. **Trade-offs**: the alternatives worth considering and what each would change.
5. **Open questions**: what the user needs to decide or find out before building.

Be specific to their proposal and constraints, not generic. If information is missing, say what you'd need. When artifacts are available and the user asks for a write-up, put it in a Markdown document artifact.
