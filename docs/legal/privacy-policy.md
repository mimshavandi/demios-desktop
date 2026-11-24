# Demios Desktop privacy policy

> **Draft: not yet approved by the owner. Contact details and controller identity still need to be filled in.**
> Task T-STORE-05 (store readiness audit, finding S-04). Every statement below was checked against the code on `main` (`d5f9463`). Items marked *(verify)* still need checking before this is published. Notes for the owner are in block quotes and come out before publishing.

Last updated: `<date>`

## Who we are

Demios Desktop ("the app") is an open-source desktop app licensed under Apache-2.0. It is published by `<publisher / controller name>`, `<postal address>`. App id: `io.demios.app`.

> Owner: `main` still uses the id `io.demios.desktop`. D-022 changes it to `io.demios.app` (branch `t/T-CORE-06-app-id`). The folder and keychain names below assume that change has been merged *(verify before publishing)*.

## The short version

- **We collect nothing.** The app has no account, no analytics, no crash reporting and no Demios server. Nothing you do in the app is sent to us.
- Your chats, settings and tool history stay on your computer.
- Your API keys and tokens are kept in your operating system's keychain.
- When you choose an online AI provider, a model download or a tool server, the app connects **directly** to that service for you. That service's own terms and privacy policy apply.

## What the app collects

Nothing reaches us. The app has no code that sends usage data, crash reports or identifiers to the publisher.

Settings shows a switch called "Share anonymous usage statistics". It is off by default and it does nothing: the app has no telemetry code to turn on. We will remove the switch, or replace it with a separate opt-in that this policy describes before it ships.

## What stays on your device

The app keeps its data in a SQLite database called `demios.sqlite3`, in your operating system's app-data folder:

| System | Folder |
|---|---|
| macOS | `~/Library/Application Support/io.demios.app/` |
| Windows | `%LOCALAPPDATA%\io.demios.app\` (in the Microsoft Store version, Windows may redirect this folder into the app's package folder *(verify)*) |
| Linux | `$XDG_DATA_HOME/io.demios.app/`, usually `~/.local/share/io.demios.app/` (in the Flatpak version, under `~/.var/app/io.demios.app/` *(verify)*) |

The database holds:

- **Conversations:** title, messages, the model that answered, and timestamps.
- **Settings:** which model fills each slot (chat, fast, embeddings) and the MCP tool servers you set up. It stores only the *names* of their credentials, never the credentials themselves.
- **Tool history ("side-effect ledger"):** for each tool call the app makes, the tool name, the arguments, the result or error, and timestamps. This history lets the app avoid repeating an action by accident.

Other things stored locally:

- **Interface preferences** (for example, whether the sidebar is open) in the app window's own browser storage *(verify the folder for each system)*.
- **Model files**, once you download local models (today only the `dn` command-line tool does this): `demios/models/` inside the same base folder, or the folder set in `DN_MODELS_DIR`.
- **Artifacts** (documents and apps the model writes) will be saved in the same database once the artifact feature ships in the app. The current build does not store them yet.

Nothing in this folder is uploaded anywhere by the app.

## Keys and secrets

API keys for AI providers, and the OAuth or bearer tokens for tool servers, are stored in your system keychain: macOS Keychain, Windows Credential Manager, or the Secret Service on Linux (for example GNOME Keyring or KWallet). They are stored under the service name `io.demios.app`.

They are never written to the database or to logs, and the app's interface never shows them back to you. Local tool programs the app starts do not receive them in their environment, except a credential you have explicitly assigned to that server.

## What goes to third parties you choose

The app only contacts these services when you set them up or start an action. Each one receives the data described here, under **its own** terms and privacy policy. We have no access to it and are not responsible for how they handle it.

### AI providers (bring your own key)

When you add a key for Anthropic, OpenAI or Google Gemini and use one of their models, the app sends requests straight from your computer to that provider (`api.anthropic.com`, `api.openai.com`, `generativelanguage.googleapis.com`), under your account with them. A request contains:

- the messages in the conversation, including any tool results in it,
- the list of tools available to the model,
- your API key, which the provider uses to identify you.

Background tasks can also send conversation content to the model in your "fast" slot, for example to write a chat title. The app may check your key by asking the provider for its list of models.

Provider policies: Anthropic `<link>`, OpenAI `<link>`, Google `<link>`.

If you use a **local model** instead, the conversation is processed on your computer and is not sent anywhere.

> Owner: the provider code (`dn-providers`) can also talk to any OpenAI-compatible URL. The shell does not offer this yet. Once it does, add a line: "an endpoint you enter receives the same data".

### Hugging Face (model downloads)

Local model files are downloaded directly from Hugging Face (`huggingface.co`). Like any web server, Hugging Face sees your IP address, the file you asked for and standard request headers. No Demios account or Hugging Face account is needed, and the app sends no identifier of its own *(verify the user agent string)*. Before a download, the app will show the model's licence and a link to its model card (D-020; not in the app yet). Hugging Face privacy policy: `<link>`.

> Owner: the store audit mentions `HF_ENDPOINT` mirrors, but the code on `main` has no `HF_ENDPOINT` handling; downloads use fixed `huggingface.co` URLs. Add a mirror sentence only when that support ships.

### MCP tool servers

You can connect MCP tool servers, which let the model use tools. Two kinds exist:

- **Local servers** are programs on your computer that the app starts for you (for example with `npx` or `uvx`). They can read files and reach the internet on their own, according to what the program does. Only install servers you trust.
- **Remote servers** are online services. The app connects to them over HTTPS. When a server needs it, the app signs you in through your browser with OAuth, and the server sees the app as the client "Demios Desktop".

When the model calls a tool, the server receives the **arguments of that call**. These can contain parts of your conversation. The tool's answer comes back into the conversation, so it is also sent to your AI provider if you use one.

Tools that change things ask for your approval first: either every time, or once per chat for low-risk changes. Tools that a server marks as read-only can run **without asking**, so their arguments are sent without a prompt. You can switch any tool to "always ask" or turn it off. Each server's own policy applies.

> Owner: the brief said "MCP servers receive the arguments of tool calls the user approves". That is not exact, because read-only tools run automatically (`RiskTier::default_approval`, `docs/security/mcp-tools.md` T1). The text above describes what the code does. If you change the default, change this text too.

### Links in answers

Today, links in model answers do nothing when you click them. The app never opens them on its own. Once links open in your browser, the policy of the website you visit applies.

## Children

The app is not meant for children under 13 (or the minimum age in your country). We collect no data from anyone, including children. The AI model you choose can produce content that is not suitable for children: the app does not filter it.

## Your choices and how to delete data

Because your data is on your own computer, you control it:

- **Delete a chat:** use Delete in the sidebar. You can undo this until you next start the app. At the next start, its messages are removed for good. *Today the chat's title and model name stay in the database as a marker, and tool history for that chat is not removed. This is being fixed: after the fix only the chat's internal id and the deletion time are kept (no title, model or content), so a future team-sync feature can tell other devices the chat was deleted; "Delete everything" removes that too. (verify once T-STORE-11 lands)*
- **Remove an API key:** go to Settings › Provider keys and click Remove. The key is removed from the keychain straight away. For 15 seconds it is held in memory so you can undo.
- **Remove a tool server's credentials:** there is no screen for this yet *(verify)*. Remove the `mcp/…` entries for `io.demios.app` in your keychain.
- **Delete everything:** there is no "delete all data" button yet *(planned)*. For now, quit the app, then:
  1. delete the app-data folder listed above (and the `demios/models` folder, if you downloaded models),
  2. delete the keychain entries with the service name `io.demios.app` (macOS: Keychain Access; Windows: Credential Manager; Linux: Seahorse or your keyring tool).
- **Uninstalling** removes the app but not this folder or your keychain entries. Delete them as described above.
- **Data held by providers, Hugging Face or tool servers** is controlled by them. Use their tools or contact them.

### GDPR and similar laws

We do not process your personal data, because nothing reaches us. Everything stays on your device or goes directly to services you choose, which are responsible for it under their own policies. If you contact us (for example by email or a GitHub issue), we use what you send only to answer you. *(Owner: state the legal basis, retention period and the right to complain to a supervisory authority, once the controller is named.)*

## Changes to this policy

If the app starts sending any data to us (for example an update check or optional crash reports), we will update this policy **before** that version is released and describe the change in the release notes. Anything like that will be opt-in unless the policy says otherwise. The latest version is always at `<policy URL>`, and earlier versions are in the project's public repository.

> Owner: a planned update check (T-STORE-08, direct downloads only) would contact a release server and reveal your IP address and app version. Add it here before it ships.

## Contact

`<contact email>` · `<postal address>` · Issues: `<GitHub issues URL>`

For security problems, see `SECURITY.md` in the repository.
