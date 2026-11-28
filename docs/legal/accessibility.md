# Almend Desktop accessibility statement

Last reviewed: set on publication · App version: 0.1.0

Almend Desktop is published by DigiNote, Inc.

## Our goal

We want Almend Desktop to be usable by everyone, including people who use a keyboard only, screen readers, magnification or high-contrast settings. Our target is **WCAG 2.2 level AA**, applied to a desktop app (see EN 301 549, chapter 11). The app **does not claim full conformance yet**. This page says what we check today and what we know is missing.

## What we do today

**Design rules** (`docs/design/design-system.md` §7):

- Every control has an accessible name, role and state.
- A streaming answer is announced once, politely, when it is finished, not word by word. The app uses a hidden `aria-live="polite"` status region for this.
- Focus moves on purpose: a new chat puts focus in the message box, closing a dialog returns focus to the button that opened it, and a new approval card takes focus unless you are typing.
- The interface language is declared (`lang="en"`).
- Keyboard shortcuts (on Windows and Linux, use `Ctrl` instead of `⌘`):

  | Shortcut | Action |
  |---|---|
  | `⌘N` | New chat |
  | `⌘\` | Show or hide the sidebar |
  | `⌘,` | Settings |
  | `⌘.` | Stop the answer being written |
  | `⌘⇧C` | Copy the last answer |

  On an approval card, `⌘↵` allows once and `Esc` denies. In Settings, the Left and Right arrow keys move between tabs.

**Automated checks:**

- An end-to-end test runs the real app on Linux and Windows, in CI. It sends a message with the keyboard (`Enter`), stops the answer with `⌘.`/`Ctrl+.`, and runs **axe-core** on the screen. The test fails on any serious or critical axe finding.
- UI unit tests find controls by their accessible role and name. This catches missing labels.

**Before each release:** we walk through the main flow (start a chat, send, stop, approve a tool call, change settings) using only the keyboard.

## Known gaps

- **macOS:** WKWebView has no WebDriver, so all macOS checks are manual, from a release checklist.
- **Screen readers:** no full pass has been done yet with VoiceOver (macOS), NVDA or Narrator (Windows), or Orca (Linux). We won't claim the app is accessible until a full screen-reader pass is done.
- **Generated content:** answers and artifacts (pages, apps and diagrams the model writes) are written by the AI model you choose. We cannot guarantee that they are accessible: an artifact may lack alt text, headings, labels or enough contrast. You can ask the model to fix this.
- **Artifacts on Windows** are shown in a separate view on top of the pane. Screen-reader and focus behaviour there hasn't been checked by hand yet. Tab moves from an artifact back into the app.
- **Not yet checked:** high-contrast and forced-colours modes, zoom to 200%, and reduced motion.

## Report a problem

If something in the app is hard or impossible to use, please tell us at hello@almend.ai, or use **Report a problem** in **Settings → General → About**. Tell us your operating system, any assistive technology you use, and what you were trying to do. We aim to reply within 72 hours.

This statement is voluntary and is not a legal declaration of conformity.
