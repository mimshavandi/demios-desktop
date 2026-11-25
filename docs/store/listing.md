# Store listing copy (draft, T-STORE-12)

**Status: draft for the owner to approve.** It describes only what the current build does (audit S-07: no future functionality in the listing). Update it when a feature ships, not before.

The same text is in `apps/desktop/tauri.conf.json` (`bundle.shortDescription`, `bundle.longDescription`). Installers (MSI/NSIS properties, DMG, .deb/.rpm) and later MSIX and AppStream metainfo read it from there. Keep both in sync.

## Name

Demios Desktop

## Short description (≤ 100 characters)

Chat with AI models on your own computer, or with your own provider API keys.

## Long description

Demios Desktop is an open-source app for chatting with AI models.

- **Run models on your computer.** Download a model from the built-in list, or GGUF models from Hugging Face, and chat with them through llama.cpp. The app shows whether it fits in your computer's memory and its licence before you download it.
- **Or bring your own key.** Add an API key for Anthropic, OpenAI or Google Gemini and chat with their models under your own account and their terms.
- **See what the model makes.** Web pages, small apps, diagrams and documents open next to the chat, run sealed off from your computer, and keep every version.
- **Give models tools.** Add tool servers (MCP) that run on your computer or online. Tools that change something show exactly what they will do and wait for your OK, and a program on your computer starts only after you allow it.
- **Your data stays with you.** There is no Demios account and no Demios server. Chats and settings stay on your computer, keys stay in your system keychain, and the app collects no usage data. Delete all local data removes everything the app stored, in one step.

Answers come from the AI model you chose and can be wrong. Check important facts.

## Category

Productivity

## Not in the listing yet

These wait until they ship in the build being listed: nothing at the moment.

## Bundle fields

| Field | Value | Who decides |
|---|---|---|
| `copyright` | Copyright © 2026 DigiNote Inc. Licensed under the Apache License 2.0. | Set (owner) |
| `category` | Productivity | Set |
| `homepage` | https://github.com/mimshavandi/demios-desktop | Placeholder until the owner supplies a product URL |
| `publisher` | DigiNote Inc. | Set (owner). Must match the Partner Center publisher for MSIX |
| Support contact | mahdi@demios.io | Set (owner). Listing field only; not in `tauri.conf.json` |

## Still to do (T-STORE-12)

- **Release-build screenshots** (S-24). Capture them from a signed release build with a real local model and with a BYOK model. The existing screenshots are dev captures with the fake model and must not be used in a listing.
