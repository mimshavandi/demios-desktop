# assistant.default.system: changes

## v2 (2026-11-28)

- **The date is gone from the prompt.** The runtime adds it in the turn's recap (ADR-0020 §2.3), so the prompt's start stays the same from day to day and a provider's or llama.cpp's cache can reuse it. Before this, the date was fixed when the app started, so it was wrong after midnight.
- Otherwise unchanged from v1.

## v1

The first version.
