# Getting started

## Install

### Download the preview

A preview build of Almend Desktop 0.1.0 is available for macOS (Apple silicon and Intel), Windows 10 and 11, and Linux (AppImage, `.deb` and `.rpm`). Download it from [demios.io/download](https://demios.io/download/) or from the [GitHub release](https://github.com/mimshavandi/demios-desktop/releases/tag/v0.1.0-preview.8cd482c). The release notes say which file is for your system.

The preview isn't signed, so your system warns you the first time you open it:

- **macOS:** open the `.dmg` and drag **Demios Desktop** to Applications. Open the app once and close the warning. Then go to **System Settings → Privacy & Security**, find "Demios Desktop was blocked" and select **Open Anyway**.
- **Windows:** run the `-setup.exe`. It installs for you only and doesn't need an administrator. If SmartScreen says "Windows protected your PC", select **More info**, then **Run anyway**.
- **Linux:** make the AppImage executable (`chmod +x`) and run it, or install the `.deb` with `sudo apt install ./<file>.deb`, or the `.rpm` with `sudo dnf install ./<file>.rpm`. On openSUSE, add `--allow-unsigned-rpm` to `zypper install`.

**Settings → General → About** shows "Preview build (unsigned)" and the commit it was built from. A preview doesn't update itself. To get a newer one, install it over the old one; your chats and settings are kept.

### Or build it from source

#### What you need

- **Rust** 1.89 or newer. If you use rustup, the repository picks the right toolchain for you.
- **Node.js** 22 and npm.
- **CMake and a C/C++ compiler.** llama.cpp, which runs models on your computer, is built from source.
- **System libraries for the app window:**
  - macOS: Xcode Command Line Tools (`xcode-select --install`).
  - Linux (Debian or Ubuntu): `sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`
  - Windows: the WebView2 runtime (included in Windows 10 and 11) and the Visual Studio C++ build tools.

#### Build and open the app

```sh
git clone https://github.com/mimshavandi/demios-desktop.git
cd demios-desktop/apps/desktop
npm ci
npm run tauri dev
```

The first build takes a while because it compiles llama.cpp. For a faster, optimised app, run `npm run tauri build`; the result is in `target/release/bundle` at the root of the repository.

## Send your first message

1. When the app opens, you'll see a short notice: answers come from the AI model you choose, and they can be wrong. Select **Got it**.
2. Until a model is set up, the message box says "Connect a model to start chatting". Select **Connect a model**. This opens **Settings → Models → On this device**. You can also get there any time with **Settings** at the bottom of the sidebar.
3. Add a model in one of two ways (details in [Adding a model](models.md)):
   - **On this device:** pick a model that shows **Ready** and select **Download model**.
   - **Your own key:** paste an Anthropic, OpenAI or Google Gemini API key on the **Providers** tab and select **Save key**.
4. Go back to the chat, type a message and press Enter. Or select one of the example prompts.

The model in use is shown at the top right of the window and under each answer.

## Finding your way around

- **Settings** at the bottom of the sidebar, or ⌘, (Ctrl+, on Windows and Linux), opens Settings on the page you had open last. Its sections are listed on the left:
  - **General**: **About** and updates, **Privacy** (with the folder your chats are stored in), **Help and legal** (reporting a problem, the privacy policy, the accessibility statement and **Open-source licences**: every package shipped with the app, searchable, with its full licence text), and **Delete local data**.
  - **Models**: which model does each task, then **On this device** (download models; the folder they're stored in) and **Providers** (your API keys).
  - **Tools**: tool servers (MCP).
  - **Skills**: built-in skills and your own.
- **About** shows the app version. In a copy downloaded from GitHub, **Check for updates** looks for a new version when you select it. You can also turn on **Check for updates when Demios starts**; it's off unless you do.

## Next

- [Chats](chats.md): stop, retry, copy and delete
- [Keyboard shortcuts](keyboard-shortcuts.md)
- [Privacy and your data](privacy.md)
