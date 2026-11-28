# AI literacy for contributors

Almend Desktop runs AI models the user chooses: local GGUF models through llama.cpp, or providers the user adds a key for. This page is the short version of what contributors need to know. It is our AI-literacy measure (EU AI Act Art. 4; readiness audit S-11, T-STORE-09).

## What the models are and aren't

- A model predicts likely text. It doesn't look facts up unless a tool does it for it, and it can state wrong things confidently ("hallucinate").
- Small local models make more mistakes than large hosted ones. Quantisation (Q4, Q8, …) trades quality for memory.
- Output depends on the prompt, the agent's system prompt (skill manifests, `apps/desktop/src/agent.rs`, ADR-0005), sampling settings and the context length. The same question can get different answers.
- A model can be steered by text it reads: web pages, files and tool results can carry instructions (prompt injection). That's why tool calls go through approval (`dn-tools`, ADR-0006) and tool output is never treated as the user's request.

## What the app must always do

- **Say it's AI.**
  - The composer shows "Answers come from the AI model you chose and can be wrong. Check important facts." and is described by that line for screen readers.
  - First run shows a dismissible notice, stored as `Settings.ai_notice_seen`.
  - Settings › About repeats it.
  - Keep these words in `apps/desktop/ui/src/ai.ts`, so they are changed in one place.
- **Mark model-made content.**
  - Every assistant message stores its model ref and time (`store_messages.model`, `created_at`).
  - In the UI, an answer is an `article` labelled "AI answer from …" with `data-ai-generated="true"` and `data-generator="<model ref>"`.
  - Anything model-made that leaves the chat view gets a machine-readable marker from `ai.ts`. That covers artifacts, exports, and saved files:
    - `aiGeneratorMeta()` for HTML;
    - `aiGeneratorComment()` for SVG/XML;
    - `aiFrontMatter()` for Markdown.
  - If image generation is ever added, it needs C2PA metadata as well.
- **Show where an answer ran.** The model badge says "on this device" or names the provider. Provider rows in Settings say that messages go to that provider.
- **Side effects need approval; read-only tools may run without asking.** A model-requested action that changes something needs the user's approval, as described in the design system §4 approval card. The approval never times out into "allow". Tools marked read-only (`RiskTier::Read`) run without asking.

## Known gaps

- **Copied answers aren't marked.** "Copy answer" puts plain text on the clipboard, with no machine-readable marker. A rich-clipboard flavour (`text/html` with the generator meta) depends on `ClipboardItem` support in each OS webview, so it is left out for now.
- **Artifacts:** the app doesn't render artifacts yet. W-artifacts applies the `ai.ts` markers when it embeds the sandbox (T-ART-06).

## Open question

Whether Almend is the *provider* of a generative AI system or only a deployer-side UI, when it runs a model the user picked, is a legal question for the owner. Until it's answered, we apply the transparency duties (Art. 50) in full.
