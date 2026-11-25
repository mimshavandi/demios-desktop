# Demios Desktop

An open-source desktop app for chatting with AI models you choose: one running on your own computer, or a provider you reach with your own API key. No account needed.

## Why Demios Desktop

- **Local first.** Chats and settings stay in a database on your computer. Models from the catalogue run on your machine, and nothing leaves it.
- **Your keys stay yours.** Provider API keys are stored in your operating system's keychain, not in a file.
- **Nothing is sent to us.** There is no Demios account, server, analytics or crash reporting.
- **Open source.** Apache-2.0 licence. Read the code, build it yourself, change it.

## What it does today

- Chat with streamed answers, with Markdown and highlighted code blocks. Links in answers open in your browser after a system dialog shows the full address.
- Artifacts: web pages, small apps, diagrams and documents the model makes open in a side pane, where they run sealed off from your files, the app and the network. Go back to earlier versions, ask the model to fix errors, and allow network access per artifact if you need it.
- Stop an answer mid-way (Ctrl+. or ⌘.) and retry it.
- Group chats in projects, with instructions that apply to every chat in the project and files you add to it. Answers in a project use passages from its files, and each answer lists its sources.
- Choose a skill for a chat, such as Summarize or Write email, or write your own. A skill adds instructions and can only narrow which tools the model may use; it never skips an approval.
- Keep conversations between launches, and delete them with Undo.
- Run models on your computer: download from a built-in catalogue of 13 open models: 11 for chat and 2 embedding models. Each row shows whether the model fits your machine ("Won't fit" when it doesn't), its licence, and download progress. Downloads can be paused, resume after an interruption, and are checked against their SHA-256.
- Add GGUF models from Hugging Face: search, see each version's size, whether it fits and its licence before downloading, and add a Hugging Face token for gated models. GGUF models already in your Hugging Face cache are used where they are, never changed or deleted. A `demios://models/add` link opens the same check in the app; nothing downloads until you choose to.
- Use your own key for Anthropic, OpenAI or Google Gemini.
- Light and dark appearance that follows your system, and keyboard shortcuts for the main actions.
- See where your data goes in **Settings → General**, and remove everything with **Delete all local data**: chats, settings, downloaded models and the keys Demios saved, after a confirmation that lists what will go.
- A notice on first run, and a line under the message box, say that answers come from the model you chose and can be wrong.
- Report a problem, or a single answer, through a prefilled form on GitHub. Nothing from your chats is sent.
- Read the licence of every package shipped with the app, searchable, in **Settings → General → About → Open-source licences**.
- Open the privacy policy and the accessibility statement from **Settings → General → About**.
- An answer from a provider that stops responding ends with an error you can retry, instead of waiting forever.
- Add tool servers (MCP) in **Settings → Models → Tool servers**: a program on this computer, which starts only after you allow its exact command, or a remote HTTPS server, with sign-in in your browser. Tools that change something show their exact arguments and wait for you to allow or deny them. See each server's tools and their risk, choose per tool whether it asks, and review new or changed tools before they run without asking.
- The `dn` command-line tool: run a model, check whether it fits, browse and download models from Hugging Face, and open `demios://` add-model links.

## Coming next

Not available yet:

- Connecting the app to an OpenAI-compatible server (such as Ollama). The `dn` CLI can already do this.
- Signed installers, on GitHub Releases and demios.io: `.dmg` for macOS, `.exe` for Windows, and AppImage, `.deb` and `.rpm` for Linux.
- Updates in the app for those installers. **Settings → General → About → Check for updates** is built, and starts working with the first signed release.

Versions in app stores (Mac App Store, Microsoft Store, Flathub) are not planned for the first releases.

## Screenshots

Screenshots coming with the first release.

## Install

A **preview build** of Demios Desktop 0.1.0 is available for macOS, Windows and Linux. Download it from [demios.io/download](https://demios.io/download/) or from the [GitHub release](https://github.com/mimshavandi/demios-desktop/releases/tag/v0.1.0-preview.454e01b).

| System | File |
|---|---|
| macOS 11 or later, Apple silicon | `…-macos-arm64.dmg` |
| macOS 11 or later, Intel | `…-macos-x64.dmg` |
| Windows 10 or 11 (x64) | `…-windows-x64-setup.exe` |
| Linux (x86_64), any distribution | `…-linux-x86_64.AppImage` |
| Debian, Ubuntu (amd64) | `…-linux-amd64.deb` |
| Fedora, openSUSE (x86_64) | `…-linux-x86_64.rpm` |

**The preview builds aren't signed**, so your system warns you the first time you open the app:

- **macOS:** open the `.dmg` and drag **Demios Desktop** to Applications. Open it once and close the warning, then go to **System Settings → Privacy & Security** and select **Open Anyway**.
- **Windows:** run the `-setup.exe`; it installs for you only, without an administrator. If SmartScreen says "Windows protected your PC", select **More info → Run anyway**.
- **Linux:**
  - AppImage: `chmod +x Demios-Desktop-*.AppImage`, then run it. If it says FUSE is missing, install `libfuse2` (`libfuse2t64` on Ubuntu 24.04).
  - `.deb`: `sudo apt install ./Demios-Desktop-0.1.0-preview-linux-amd64.deb`.
  - `.rpm`: `sudo dnf install ./Demios-Desktop-0.1.0-preview-linux-x86_64.rpm`. On openSUSE, use `sudo zypper install --allow-unsigned-rpm ./Demios-Desktop-0.1.0-preview-linux-x86_64.rpm`.

The release lists every file's SHA-256 in `SHA256SUMS.txt`, and its notes have the full steps. A preview build doesn't update itself. To get a newer one, install it over the old one; your chats and settings are kept.

### Build from source

To build Demios Desktop yourself instead:

#### Prerequisites

- **Rust** 1.89 or newer (stable toolchain with `rustfmt` and `clippy`; `rust-toolchain.toml` selects it if you use rustup).
- **Node.js** 22 and npm.
- **CMake and a C/C++ compiler**: llama.cpp, which runs local models, is compiled from source.
- **Platform webview libraries:**
  - macOS: Xcode Command Line Tools (`xcode-select --install`).
  - Linux (Debian/Ubuntu): `sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`
  - Windows: the WebView2 runtime (included with Windows 10 and 11) and the Visual Studio C++ build tools.

#### Build and run

```sh
git clone https://github.com/mimshavandi/demios-desktop.git
cd demios-desktop/apps/desktop
npm ci

npm run dev:app      # app window with a built-in test model, no key or download needed
npm run tauri dev    # app window with no model: connect one in Settings
npm run tauri build  # optimised build; the output is in target/release/bundle
```

The test model in `dev:app` gives canned answers. It exists only in debug builds and is never part of an optimised build.

To try the app without touching your normal data, use a throwaway data folder: `DEMIOS_DATA_DIR=/tmp/demios-profile npm run dev:app`.

## Getting started

The [user guide](docs/user/README.md) covers everything below in more detail.

Open **Settings → Models** (select **Models** at the bottom of the sidebar) and pick one of these:

- **On this device:** choose a model from the catalogue and select **Download model**. Check the fit status first: it tells you whether the model fits this computer's memory. When the download finishes, the app picks a ready model for chat unless you choose one for the Chat slot.
- **Your own key:** add an Anthropic, OpenAI or Google Gemini API key. The key goes into your system keychain; macOS may ask you to allow keychain access.

### The `dn` command-line tool

Build it from the repository root with `cargo install --path apps/cli`, or run it with `cargo run -p dn -- <command>`.

```sh
dn models list                                   # catalogue models and which are downloaded
dn models search qwen                            # search Hugging Face for GGUF models
dn models pull qwen3-4b-q4_k_m                   # download a catalogue model (resumable, verified)
dn fit qwen3-4b-q4_k_m                           # will it fit this machine, and with which settings
dn run -m qwen3-4b-q4_k_m "Explain DNS briefly"  # chat from the terminal
dn run --endpoint http://localhost:11434/v1 -m llama3.2 "Hi"   # an OpenAI-compatible server
```

Models downloaded with `dn`, from the catalogue or from Hugging Face, show up in the app too. See [the dn guide](docs/user/dn-cli.md), or run `dn --help` for every option.

## Privacy and safety

When you chat with a model on this device, nothing leaves your computer. When you use a provider key, your messages in that chat go to that provider under your own account with them. Browsing and downloading models contacts Hugging Face. A tool server you add receives what its tools are asked to do, which can include text from your chat. Tools that change something ask first: the call shows its exact arguments and waits for you to allow or deny it. Tools a server marks as read-only run without asking. More detail: [Tools and approvals](docs/user/tools-and-approvals.md), [the MCP threat model](docs/security/mcp-tools.md) and the [store readiness audit](docs/store/readiness-audit.md) (data-collection section).

## Project status

Early and pre-release (version 0.1.0). Expect rough edges and changes between versions. What changed is in the [changelog](CHANGELOG.md). CI builds and tests it on macOS, Linux and Windows; the end-to-end app tests run on Linux.

## Contributing

Contributions are welcome. Start with [CONTRIBUTING.md](CONTRIBUTING.md), then the [architecture decisions](docs/adr/README.md) and the [design system](docs/design/design-system.md). The [docs index](docs/README.md) lists everything else.

## Security

Please report vulnerabilities privately, as described in [SECURITY.md](SECURITY.md). Don't open public issues for security problems.

## Licence

[Apache-2.0](LICENSE).
