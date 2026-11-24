# Changelog

All notable changes to Demios Desktop are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). [docs/releasing.md](docs/releasing.md) explains how this file becomes the release notes.

Only changes people using the app or the `dn` command-line tool can notice are listed. Each entry links the pull request that made it.

## [Unreleased]

Nothing has been released yet. This section lists what is on `main` now.

### Added

#### Desktop app

- Chat with a model, with answers streamed as they are written, Markdown formatting and highlighted code blocks ([#14](https://github.com/mimshavandi/demios-desktop/pull/14)).
- Stop an answer with ⌘. (Ctrl+. on Windows and Linux), retry the last answer, and copy any answer. ⌘⇧C copies the last answer ([#14](https://github.com/mimshavandi/demios-desktop/pull/14)).
- Chats are kept on your computer between launches. Deleting a chat can be undone for 10 seconds ([#1](https://github.com/mimshavandi/demios-desktop/pull/1), [#14](https://github.com/mimshavandi/demios-desktop/pull/14)).
- Keyboard shortcuts: ⌘N for a new chat, ⌘, for Settings, ⌘\ to show or hide the sidebar. On Windows and Linux, use Ctrl instead of ⌘ ([#14](https://github.com/mimshavandi/demios-desktop/pull/14)).
- Light and dark appearance that follows your system setting ([#3](https://github.com/mimshavandi/demios-desktop/pull/3), [#14](https://github.com/mimshavandi/demios-desktop/pull/14)).
- Use your own API key for Anthropic, OpenAI or Google Gemini. The key is stored in your system keychain, and removing it can be undone ([#4](https://github.com/mimshavandi/demios-desktop/pull/4), [#8](https://github.com/mimshavandi/demios-desktop/pull/8), [#14](https://github.com/mimshavandi/demios-desktop/pull/14)).
- Choose which model answers in chats and which one does quick background tasks, in **Settings → Models** ([#14](https://github.com/mimshavandi/demios-desktop/pull/14)).
- Run models on your computer: download one of 12 open models from a built-in catalogue. Each shows whether it fits your computer's memory, its licence and a link to its model card. Downloads show progress, can be paused and resumed, pick up again after an interruption, and are checked against their published SHA-256 before use ([#12](https://github.com/mimshavandi/demios-desktop/pull/12), [#28](https://github.com/mimshavandi/demios-desktop/pull/28)).
- **Settings → General** shows where your data goes: what stays on your computer, what goes to a provider you add a key for, and what goes to Hugging Face ([#40](https://github.com/mimshavandi/demios-desktop/pull/40)).
- **Delete all local data** removes chats, settings, tool history, downloaded models and the keys Demios saved in your keychain, after a confirmation that lists what will go ([#40](https://github.com/mimshavandi/demios-desktop/pull/40)).
- A notice on first run, and a line under the message box, say that answers come from the AI model you chose and can be wrong. Answers are marked as AI-generated with the model that wrote them ([#39](https://github.com/mimshavandi/demios-desktop/pull/39)).
- **Settings → General → About → Open-source licences** lists every package shipped with the app (Rust crates, npm packages and bundled C/C++ code). You can search by name or licence and read each full licence text ([#45](https://github.com/mimshavandi/demios-desktop/pull/45)).
- Add any GGUF model from Hugging Face in **Settings → Models → On this device → Add from Hugging Face**: search, see each version's size, fit and licence before downloading, and save a Hugging Face token (kept in the keychain, sent only to Hugging Face) for gated models. Downloads are checked against the published checksum, can be paused and resumed, and a version without a checksum can't be downloaded. Models downloaded from Hugging Face with `dn`, and GGUF models in your Hugging Face cache, show up too; cache files are used where they are and never changed or deleted ([#73](https://github.com/mimshavandi/demios-desktop/pull/73)).
- Add tool servers (MCP) in **Settings → Models → Tool servers**: a program on this computer or a remote HTTPS server, with secrets kept in the keychain. A local program starts only after you allow its exact command in a system dialog. Remote servers can use sign-in in your browser. Servers can be edited, reconnected and removed. Tools that change something ask for your approval in the chat first ([#7](https://github.com/mimshavandi/demios-desktop/pull/7), [#22](https://github.com/mimshavandi/demios-desktop/pull/22), [#65](https://github.com/mimshavandi/demios-desktop/pull/65)).
- **Report a problem** (Settings → General) and **Report this answer** (the flag under an answer) open a prefilled form on GitHub in your browser ([#42](https://github.com/mimshavandi/demios-desktop/pull/42)).

#### `dn` command-line tool

- `dn run` chats with a model from the terminal: a downloaded GGUF file, a catalogue model, or an OpenAI-compatible server with `--endpoint` ([#12](https://github.com/mimshavandi/demios-desktop/pull/12)).
- `dn fit` tells you whether a model fits this machine and which settings to use. `dn bench` measures speed and peak memory ([#12](https://github.com/mimshavandi/demios-desktop/pull/12)).
- `dn run` and `dn bench` pick context size, cache type and GPU layers from `dn fit` unless you set them. A model that won't fit is refused unless you pass `--force`; `--no-auto` keeps the plain llama.cpp defaults ([#35](https://github.com/mimshavandi/demios-desktop/pull/35)).
- `dn models list` and `dn models pull` list and download catalogue models ([#12](https://github.com/mimshavandi/demios-desktop/pull/12)).
- `dn models search`, `dn models files` and `dn models pull <owner/name> --quant <quant>` find and download GGUF models from Hugging Face at a pinned revision ([#30](https://github.com/mimshavandi/demios-desktop/pull/30)).
- `dn models open <link>` reads a `demios://models/add?…` link, shows the Hugging Face model it points at, its licence and whether it fits this machine, and downloads it only after you type `y`. The app doesn't open these links yet ([#34](https://github.com/mimshavandi/demios-desktop/pull/34)).
- `dn models list` also shows models you downloaded from Hugging Face and the ones already in your Hugging Face cache. The cache is only read, never changed ([#33](https://github.com/mimshavandi/demios-desktop/pull/33)).
- With `--endpoint`, `dn run` detects llama.cpp's `llama-server`, vLLM and Ollama and switches the model's thinking on or off in the way that server expects ([#44](https://github.com/mimshavandi/demios-desktop/pull/44)).

### Changed

- An answer from a provider or server that stops sending now ends with a message you can retry: after 5 minutes with no first response, or 60 seconds of silence once the answer has started ([#44](https://github.com/mimshavandi/demios-desktop/pull/44)).

### Security

- Provider API keys are kept in the system keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service), never in the database or logs ([#4](https://github.com/mimshavandi/demios-desktop/pull/4)).
- Every downloaded model file is checked against its SHA-256 before use. A Hugging Face file without a published checksum isn't downloaded ([#12](https://github.com/mimshavandi/demios-desktop/pull/12), [#30](https://github.com/mimshavandi/demios-desktop/pull/30)).
- Your Hugging Face token is sent only to Hugging Face, not to the download servers it redirects to ([#30](https://github.com/mimshavandi/demios-desktop/pull/30)).
- Problem and answer reports carry only the app version, your operating system and, for an answer, the model's name. No chat content is sent; you add what you choose in the form ([#42](https://github.com/mimshavandi/demios-desktop/pull/42)).

[Unreleased]: https://github.com/mimshavandi/demios-desktop/commits/main
