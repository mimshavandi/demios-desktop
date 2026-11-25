# Reviewer notes (draft)

Draft notes for store certification. They follow the six-section structure of the app-store-compliance skill's `templates/REVIEW-NOTES-TEMPLATE.md` (pinned `ad76746e`), rewritten for a desktop app with no account. Use them as:

- **Microsoft Store:** Partner Center › Submission › "Notes for certification".
- **Flathub:** the submission PR description, plus the `finish-args` reasons in the manifest.
- **macOS direct:** not reviewed by Apple beyond notarization, so they serve as the release checklist.

**Before submitting:**

- Replace every `<…>`.
- Delete paragraphs about features that are not in the submitted build (see audit S-07).
- Never put a key, password or account id in this file or anywhere in the repo. Anything secret goes only into the store's notes field.

```text
1. SCREEN RECORDING
Demo video: <URL to a screen recording of THIS release build on a clean machine>
The recording shows:
- First launch with no model connected ("Connect a model" state)
- Downloading the small default local model (Qwen2.5 0.5B, about 0.5 GB) and chatting with it offline
- Adding a provider key (BYOK) and chatting with a cloud model
- An artifact (HTML/SVG/Markdown) opening in the side pane
- An MCP tool call: the approval prompt showing the exact arguments, then Allow / Deny
- Settings > About: version, open-source licences. The privacy policy link is added once the policy is published.
There are no accounts, purchases, ads or user-to-user content, so those flows do not exist.

2. APP PURPOSE
Demios Desktop is a free, open-source (Apache-2.0) AI chat app that runs on your own computer.
You pick the model: a local model that runs on this machine (llama.cpp), or a cloud model through your own API key with a provider you already use.
Chats and settings are stored locally. Keys are stored in the operating system's credential store.
There is no Demios account, no Demios server and no telemetry.

3. ACCESS INSTRUCTIONS
No sign-in is needed. To reach the core feature:
  Option A, local model (no key, works offline after the download):
    1. Launch the app. Click "Connect a model".
    2. Under Local models, choose the smallest entry (about 0.5 GB) and click Download.
       The licence and model card are shown before the download starts.
    3. When it finishes, type a message in the composer and press Enter.
  Option B, cloud model with a test key:
    1. Settings > Models > Provider keys > <Provider> > Add key.
    2. Paste the test key below, save, pick a <Provider> model, and chat.
    Test key: <only if the owner provides one; spend-capped and valid until <date>, otherwise delete Option B>
  Artifacts: ask "Make an HTML page with a bouncing ball". The result opens in the right-hand pane.
  Tools (optional): Settings > Tools > Add server > HTTP, URL <a public read-only MCP test server>, then ask a question that uses it.
    Each tool that changes anything asks for approval first and shows the exact arguments.

4. EXTERNAL SERVICES
The app talks only to services the user configures or starts. There is no Demios backend.
Service / Purpose
Hugging Face (huggingface.co, or an HF_ENDPOINT mirror) / browsing and downloading open model files the user picks
The user's own AI provider (OpenAI, Anthropic, Google Gemini, or a custom OpenAI-compatible URL) / chat replies, sent with the user's own key
MCP servers the user adds (HTTP, or local programs the user installed) / tools, called only after the approval rules above
OAuth servers of those MCP servers / sign-in to the user's own account there; the redirect comes back to 127.0.0.1 on a random port
<Update server URL, direct-download builds only; Store and Flathub builds have no self-updater>

How code and content run (for questions about dynamic code):
- The app never downloads or runs executables by itself. Model files are data (GGUF weights) read by the built-in engine.
- Artifacts the model writes (HTML/JS/SVG) render in a sandboxed iframe with an opaque origin and no access to the app, files or the app's IPC.
  Their content security policy blocks all network access. Network can be enabled per artifact, only after two clicks that name the risk.
- MCP servers are programs or services the user chose to add, like an editor's extensions. The app starts them only when the user configures them,
  and every tool call goes through risk tiers and an approval prompt.

5. REGIONAL DIFFERENCES
The app works the same in every region. Which cloud providers work depends on the user's own provider account, not on the app.

6. REGULATED INDUSTRY DOCUMENTATION
Not applicable. The app gives no medical, legal or financial service. Answers come from the model the user chose
and can be wrong; the app says so next to the message box.
```

## Store-specific additions

**Microsoft Store**

- Restricted capability `runFullTrust`. Reason: "Win32 desktop app (Tauri/WebView2). It needs full trust to start local AI inference and the local tool servers (MCP) the user configures."
- Age rating (IARC): answer honestly that the app shows unfiltered text from user-chosen AI models and can display links. There is no user-to-user communication.
- Privacy policy URL: https://demios.io/privacy (live once the owner publishes it). Support contact: mahdi@demios.io (DigiNote Inc.).

**Flathub** (one `finish-args` justification per line in the PR)

- `--share=network`: AI providers, Hugging Face downloads, HTTP MCP servers.
- `--socket=wayland` and `--socket=fallback-x11`, `--share=ipc`, `--device=dri`: WebKitGTK UI and GPU inference.
- `--talk-name=org.freedesktop.secrets`: store provider keys in the user's keyring (the keyring library has no Secret-portal support yet).
- MCP: say plainly which option the build uses (audit S-12). Under option A, local stdio MCP servers outside the sandbox are not available in the Flathub build, and the app says why.
