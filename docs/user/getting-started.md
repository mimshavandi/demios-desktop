# Getting started

## Install

There are no installers yet. To use Demios Desktop now, build it from source.

### What you need

- **Rust** 1.89 or newer. If you use rustup, the repository picks the right toolchain for you.
- **Node.js** 22 and npm.
- **CMake and a C/C++ compiler.** llama.cpp, which runs models on your computer, is built from source.
- **System libraries for the app window:**
  - macOS: Xcode Command Line Tools (`xcode-select --install`).
  - Linux (Debian or Ubuntu): `sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`
  - Windows: the WebView2 runtime (included in Windows 10 and 11) and the Visual Studio C++ build tools.

### Build and open the app

```sh
git clone https://github.com/mimshavandi/demios-desktop.git
cd demios-desktop/apps/desktop
npm ci
npm run tauri dev
```

The first build takes a while because it compiles llama.cpp. For a faster, optimised app, run `npm run tauri build`; the result is in `target/release/bundle` at the root of the repository.

## Send your first message

1. When the app opens, you'll see a short notice: answers come from the AI model you choose, and they can be wrong. Select **Got it**.
2. Until a model is set up, the message box says "Connect a model to start chatting". Select **Connect a model**. This opens **Settings → Models**. You can also get there any time with **Models** at the bottom of the sidebar.
3. Add a model in one of two ways (details in [Adding a model](models.md)):
   - **On this device:** pick a model that shows **Ready** and select **Download model**.
   - **Your own key:** paste an Anthropic, OpenAI or Google Gemini API key under **Provider keys** and select **Save key**.
4. Go back to the chat, type a message and press Enter. Or select one of the example prompts.

The model in use is shown at the top right of the window and under each answer.

## Finding your way around

- **Models** at the bottom of the sidebar opens **Settings → Models**: models on this device, provider keys, and which model answers.
- **Settings** at the bottom of the sidebar, or ⌘, (Ctrl+, on Windows and Linux), opens **Settings → General**: privacy, deleting your data, reporting a problem, and **About**.
- **About** shows the app version, where your chats and models are stored, and **Open-source licences**: every package shipped with the app, searchable, with its full licence text.

## Next

- [Chats](chats.md): stop, retry, copy and delete
- [Keyboard shortcuts](keyboard-shortcuts.md)
- [Privacy and your data](privacy.md)
