# Demios Desktop

An open-source desktop app for chatting with AI models you choose: one running on your own computer, or a provider you reach with your own API key. No account needed.

## Why Demios Desktop

- **Local first.** Chats and settings stay in a database on your computer. Models from the catalogue run on your machine, and nothing leaves it.
- **Your keys stay yours.** Provider API keys are stored in your operating system's keychain, not in a file.
- **Nothing is sent to us.** There is no Demios account, server, analytics or crash reporting.
- **Open source.** Apache-2.0 licence. Read the code, build it yourself, change it.

## What it does today

- Chat with streamed answers, with Markdown and highlighted code blocks.
- Stop an answer mid-way (Ctrl+. or ⌘.) and retry it.
- Keep conversations between launches, and delete them with Undo.
- Run models on your computer: download from a built-in catalogue of 12 open models. Each row shows whether the model fits your machine ("Won't fit" when it doesn't), its licence, and download progress. Downloads can be paused, resume after an interruption, and are checked against their SHA-256.
- Use your own key for Anthropic, OpenAI or Google Gemini.
- Light and dark appearance that follows your system, and keyboard shortcuts for the main actions.
- The `dn` command-line tool: run a model, check whether it fits, and browse and download models from Hugging Face.

## Coming next

Not available yet:

- Artifacts: documents, code and small apps the model makes, opened in a side pane. The pane is there but stays empty for now.
- A screen to add MCP tool servers. The tool runtime and its approval cards exist, but servers can't be added from the app yet.
- Connecting the app to an OpenAI-compatible server (such as Ollama). The `dn` CLI can already do this.
- Signed installers for macOS, Windows and Linux.

## Screenshots

Screenshots coming with the first release.

## Install

There are no installers or signed releases yet. To try Demios Desktop, build it from source.

### Prerequisites

- **Rust** 1.89 or newer (stable toolchain with `rustfmt` and `clippy`; `rust-toolchain.toml` selects it if you use rustup).
- **Node.js** 22 and npm.
- **CMake and a C/C++ compiler**: llama.cpp, which runs local models, is compiled from source.
- **Platform webview libraries:**
  - macOS: Xcode Command Line Tools (`xcode-select --install`).
  - Linux (Debian/Ubuntu): `sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`
  - Windows: the WebView2 runtime (included with Windows 10 and 11) and the Visual Studio C++ build tools.

### Build and run

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

Open **Settings → Models** and pick one of these:

- **On this device:** choose a model from the catalogue and select **Download model**. Check the fit status first: it tells you whether the model fits this computer's memory. When the download finishes, choose the model for the Chat slot.
- **Your own key:** add an Anthropic, OpenAI or Google Gemini API key. The key goes into your system keychain; macOS asks for keychain access the first time.

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

Models downloaded with `dn` show up in the app too. Run `dn --help` for every option.

## Privacy and safety

When you chat with a model on this device, nothing leaves your computer. When you use a provider key, your messages in that chat go to that provider under your own account with them. Browsing and downloading models contacts Hugging Face. Tools that change something ask first: the call shows its exact arguments and waits for you to allow or deny it. Tools a server marks as read-only run without asking. More detail: [MCP tools and approvals](docs/security/mcp-tools.md) and the [store readiness audit](docs/store/readiness-audit.md) (data-collection section).

## Project status

Early and pre-release (version 0.1.0). Expect rough edges and changes between versions. CI builds and tests it on macOS, Linux and Windows; the end-to-end app tests run on Linux.

## Contributing

Contributions are welcome. Start with [CONTRIBUTING.md](CONTRIBUTING.md), then the [architecture decisions](docs/adr/README.md) and the [design system](docs/design/design-system.md). The [docs index](docs/README.md) lists everything else.

## Security

Please report vulnerabilities privately, as described in [SECURITY.md](SECURITY.md). Don't open public issues for security problems.

## Licence

[Apache-2.0](LICENSE).
