# Changelog

All notable changes to Demios Desktop are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). [docs/releasing.md](docs/releasing.md) explains how this file becomes the release notes.

Only changes people using the app or the `dn` command-line tool can notice are listed. Each entry links the pull request that made it.

## [Unreleased]

### Added

#### Desktop app

- Projects: group chats with standing instructions that are sent with each chat in the project, and add files, which Demios copies into its data folder. Deleting a project, or a file in it, can be undone for 10 seconds ([#135](https://github.com/mimshavandi/demios-desktop/pull/135)).
- Move a chat into a project, or out of one, from the **Project** list under the message box. Undo moves it back, and the chat follows its new project's instructions from the next message ([#165](https://github.com/mimshavandi/demios-desktop/pull/165)).

#### `dn` command-line tool

- `dn bench --quality` runs a short set of prompts with answers it can check, to catch a broken chat template or quantisation. `dn bench` also measures time to first token and speed through the chat path, can benchmark an OpenAI-compatible server with `--endpoint`, and `dn bench-report <folder>` turns saved reports into a table. Reports record a model's file name, never its full path ([#138](https://github.com/mimshavandi/demios-desktop/pull/138)).

### Changed

- Models on this device now write their prompts with their own chat template, from the model file or from its Hugging Face repository. A model without a chat template can't be used for chat: its row says so, and choosing it gives "This model has no chat template, so it can't be used for chat. Pick another model." ([#150](https://github.com/mimshavandi/demios-desktop/pull/150)).

## [0.1.0-preview.454e01b] - 2026-11-25

Preview build, not signed: installers for macOS, Windows and Linux on the [GitHub release](https://github.com/mimshavandi/demios-desktop/releases/tag/v0.1.0-preview.454e01b) and [demios.io/download](https://demios.io/download/). It doesn't update itself. Everything below is in this preview.

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
- Add GGUF models from Hugging Face in **Settings → Models → On this device → Add from Hugging Face**: search, see each version's size, fit and licence before downloading, and save a Hugging Face token (kept in the keychain, sent only to Hugging Face) for gated models. Downloads are checked against the published checksum, can be paused and resumed, and a version without a checksum can't be downloaded. Models downloaded from Hugging Face with `dn`, and GGUF models in your Hugging Face cache, show up too; cache files are used where they are and never changed or deleted ([#73](https://github.com/mimshavandi/demios-desktop/pull/73)).
- `demios://models/add` links open the add-model check in the app: licence, model card and fit for each version, with the linked version first. Nothing downloads until you select Download, and other links are ignored while one is open ([#78](https://github.com/mimshavandi/demios-desktop/pull/78)).
- A **Tools** view for each tool server: every tool's description as plain text, the risk the server gives it, and whether it asks now. Choose per tool whether it asks every time, once per chat, runs without asking or is off; risky choices ask you to confirm. A tool that is new or has changed since the server first connected asks before every use until you mark it reviewed ([#84](https://github.com/mimshavandi/demios-desktop/pull/84)).
- Add tool servers (MCP) in **Settings → Models → Tool servers**: a program on this computer or a remote HTTPS server, with secrets kept in the keychain. A local program starts only after you allow its exact command in a system dialog. Remote servers can use sign-in in your browser. Servers can be edited, reconnected and removed. Tools that change something ask for your approval in the chat first ([#7](https://github.com/mimshavandi/demios-desktop/pull/7), [#22](https://github.com/mimshavandi/demios-desktop/pull/22), [#65](https://github.com/mimshavandi/demios-desktop/pull/65)).
- Artifacts: web pages, small apps (HTML or React), SVG images, diagrams, documents and code that the model makes open in a side pane. Each change is a new version; you can view older ones and restore them, ask the model to fix reported errors, and allow network access for one artifact after a confirmation. Versions the model made are labelled AI-generated with the model's name ([#11](https://github.com/mimshavandi/demios-desktop/pull/11), [#16](https://github.com/mimshavandi/demios-desktop/pull/16), [#41](https://github.com/mimshavandi/demios-desktop/pull/41)).
- Artifacts open on Windows too ([#98](https://github.com/mimshavandi/demios-desktop/pull/98)).
- In the artifact pane, Esc closes the pane even when the artifact has focus, and the new chat, Settings and sidebar shortcuts work from inside an artifact ([#98](https://github.com/mimshavandi/demios-desktop/pull/98)).
- Choose a skill for a chat in the message box: six starter skills (compare, critique, design review, outline, summarize, write an email) add instructions for that chat. A skill can only narrow which tools the model may use, and never skips a tool approval ([#132](https://github.com/mimshavandi/demios-desktop/pull/132)).
- Links in answers open in your browser after a system dialog shows the full address ([#74](https://github.com/mimshavandi/demios-desktop/pull/74)).
- **Settings → General → About → Updates**: a copy downloaded from GitHub checks for a new version only when you select **Check for updates**, shows its release notes, and installs it after you confirm with **Install and restart**. Copies from the Microsoft Store or Flathub say that their store updates them. Builds that don't update themselves, like preview builds and builds from source, say where to get new versions ([#94](https://github.com/mimshavandi/demios-desktop/pull/94)).
- **Settings → General → About** links to the privacy policy and the accessibility statement on demios.io. Demios asks before it opens your browser ([#97](https://github.com/mimshavandi/demios-desktop/pull/97)).
- **Report a problem** (Settings → General) and **Report this answer** (the flag under an answer) open a prefilled form on GitHub in your browser ([#42](https://github.com/mimshavandi/demios-desktop/pull/42)).

#### `dn` command-line tool

- `dn run` chats with a model from the terminal: a downloaded GGUF file, a catalogue model, or an OpenAI-compatible server with `--endpoint` ([#12](https://github.com/mimshavandi/demios-desktop/pull/12)).
- `dn fit` tells you whether a model fits this machine and which settings to use. `dn bench` measures speed and peak memory ([#12](https://github.com/mimshavandi/demios-desktop/pull/12)).
- `dn run` and `dn bench` pick context size, cache type and GPU layers from `dn fit` unless you set them. A model that won't fit is refused unless you pass `--force`; `--no-auto` keeps the plain llama.cpp defaults ([#35](https://github.com/mimshavandi/demios-desktop/pull/35)).
- `dn models list` and `dn models pull` list and download catalogue models ([#12](https://github.com/mimshavandi/demios-desktop/pull/12)).
- `dn models search`, `dn models files` and `dn models pull <owner/name> --quant <quant>` find and download GGUF models from Hugging Face at a pinned revision ([#30](https://github.com/mimshavandi/demios-desktop/pull/30)).
- `dn models open <link>` reads a `demios://models/add?…` link, shows the Hugging Face model it points at, its licence and whether it fits this machine, and downloads it only after you type `y` ([#34](https://github.com/mimshavandi/demios-desktop/pull/34)).
- `dn models list` also shows models you downloaded from Hugging Face and the ones already in your Hugging Face cache. The cache is only read, never changed ([#33](https://github.com/mimshavandi/demios-desktop/pull/33)).
- With `--endpoint`, `dn run` detects llama.cpp's `llama-server`, vLLM and Ollama and switches the model's thinking on or off in the way that server expects ([#44](https://github.com/mimshavandi/demios-desktop/pull/44)).

### Changed

- An answer from a provider or server that stops sending now ends with a message you can retry: after 5 minutes with no first response, or 60 seconds of silence once the answer has started ([#44](https://github.com/mimshavandi/demios-desktop/pull/44)).
- Larger models that don't fully fit on the graphics chip can now run with part of the model in regular memory. The fit check picks this for mixture-of-experts models such as Qwen3 30B-A3B, says replies will be a little slower, and the app loads the model that way. `dn fit` shows the chosen preset ([#129](https://github.com/mimshavandi/demios-desktop/pull/129)).

### Security

- Provider API keys are kept in the system keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service), never in the database or logs ([#4](https://github.com/mimshavandi/demios-desktop/pull/4)).
- Every downloaded model file is checked against its SHA-256 before use. A Hugging Face file without a published checksum isn't downloaded ([#12](https://github.com/mimshavandi/demios-desktop/pull/12), [#30](https://github.com/mimshavandi/demios-desktop/pull/30)).
- Your Hugging Face token is sent only to Hugging Face, not to the download servers it redirects to ([#30](https://github.com/mimshavandi/demios-desktop/pull/30)).
- Artifacts run in a sandboxed frame with no access to your files, the app or the network. Network access is off until you allow it for one artifact, and turns off again when the model writes a new version ([#16](https://github.com/mimshavandi/demios-desktop/pull/16), [#41](https://github.com/mimshavandi/demios-desktop/pull/41)).
- A link in an answer opens only after you confirm it in a system dialog that shows the full address, with look-alike letters spelled out. Only http and https links can be opened ([#74](https://github.com/mimshavandi/demios-desktop/pull/74)).
- On Windows, artifacts run in a separate view that has no access to the app's commands ([#98](https://github.com/mimshavandi/demios-desktop/pull/98)).
- Keys pressed inside an artifact can only close the pane or run the new chat, Settings and sidebar shortcuts, at most one every half second. Stop and copy last answer aren't passed on, so an artifact can't fake them ([#98](https://github.com/mimshavandi/demios-desktop/pull/98)).
- An update is installed only if it is signed with the app's update key and its signature matches the version you confirmed. Microsoft Store and Flathub builds can't contain the self-updater: the build fails if it's turned on ([#94](https://github.com/mimshavandi/demios-desktop/pull/94)).
- The app's commands are available only to the app's own page, not to any other view in its window ([#85](https://github.com/mimshavandi/demios-desktop/pull/85)).
- Problem and answer reports carry only the app version, your operating system and, for an answer, the model's name. No chat content is sent; you add what you choose in the form ([#42](https://github.com/mimshavandi/demios-desktop/pull/42)).

[Unreleased]: https://github.com/mimshavandi/demios-desktop/compare/v0.1.0-preview.454e01b...main
[0.1.0-preview.454e01b]: https://github.com/mimshavandi/demios-desktop/releases/tag/v0.1.0-preview.454e01b
