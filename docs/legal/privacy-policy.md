# Demios Desktop privacy policy

> **Pending owner approval.** The owner approves the final text before it is published at demios.io/privacy and linked from the app and from demios.io. Text in [square brackets] is still to be confirmed by the owner. Every statement about the app was checked against the code on `main`.

Last updated: set on publication

## Who we are

Demios Desktop ("the app") is an open-source desktop app licensed under Apache-2.0. It is published by **DigiNote Inc.**, which is responsible for this policy and is the controller for the processing described in [Data protection (GDPR)](#data-protection-gdpr). DigiNote Inc. is also the provider of the app under the EU AI Act. App id: `io.demios.app`.

DigiNote Inc. [legal entity name and register: to be confirmed by the owner]
Niederkasseler Kirchweg 85
40547 Düsseldorf
Germany

Email: mahdi@demios.io

Data protection officer: [DPO: to be confirmed by the owner whether one is required]

This policy covers the app and the website at demios.io.

## The short version

- **We collect nothing.** The app has no account, no analytics, no crash reporting and no Demios server. Nothing you do in the app is sent to us.
- Your chats, settings and tool history stay on your computer.
- Your API keys and tokens are kept in your operating system's keychain.
- When you choose an online AI provider, a model download, a tool server or an update, the app connects **directly** to that service for you. That service's own terms and privacy policy apply.

## What the app collects

Nothing reaches us. The app has no code that sends usage data, crash reports or identifiers to the publisher, and no setting that turns any such thing on.

## What stays on your device

The app keeps its data in a SQLite database called `demios.sqlite3`, in your operating system's app-data folder:

| System | Folder |
|---|---|
| macOS | `~/Library/Application Support/io.demios.app/` |
| Windows | `%LOCALAPPDATA%\io.demios.app\` |
| Linux | `$XDG_DATA_HOME/io.demios.app/`, usually `~/.local/share/io.demios.app/` |

**Settings → General → About** in the app shows the exact folder.

The database holds:

- **Conversations:** title, messages, the model that answered, and timestamps.
- **Artifacts:** the pages, apps, diagrams and documents a model makes in a chat, every version of them, which model or whether you made each version, and timestamps.
- **Settings:** which model you chose for each task, the tool servers you added, and which local server commands you allowed to start. It stores only the *names* of their credentials, never the credentials themselves.
- **Tool history:** for each tool call the app makes, the tool name, the arguments, the result or error, and timestamps. This history lets the app avoid repeating an action by accident.

Other things stored locally:

- **Interface preferences**, for example whether the sidebar is open, in the app window's own storage.
- **Model files** you download: `demios/models/` in your user data folder (`~/Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows, `~/.local/share` on Linux), or the folder set in `DN_MODELS_DIR`. **Settings → General → About** shows it. The `dn` command-line tool uses the same folder. GGUF models the app finds in your own Hugging Face cache are used where they are; the app never changes or deletes them.

Nothing in these folders is uploaded anywhere by the app.

## Keys and secrets

API keys for AI providers, and the secrets and sign-ins for tool servers, are stored in your system keychain: macOS Keychain, Windows Credential Manager, or the Secret Service on Linux (for example GNOME Keyring or KWallet). They are stored under the service name `io.demios.app`.

They are never written to the database or to logs, and the app never shows them back to you. Local tool programs the app starts don't receive them in their environment, except a credential you have explicitly assigned to that server.

## What goes to third parties you choose

The app only contacts these services when you set them up or start an action. Each one receives the data described here, under **its own** terms and privacy policy. We have no access to it and are not responsible for how they handle it.

### AI providers (your own key)

When you add a key for Anthropic, OpenAI or Google Gemini and use one of their models, the app sends requests straight from your computer to that provider (`api.anthropic.com`, `api.openai.com`, `generativelanguage.googleapis.com`), under your account with them. A request contains:

- the messages in the conversation, including any tool results in it,
- the list of tools available to the model,
- your API key, which the provider uses to identify you.

Provider privacy policies: [Anthropic](https://www.anthropic.com/legal/privacy), [OpenAI](https://openai.com/policies/privacy-policy), [Google](https://policies.google.com/privacy).

If you use a **model on this device** instead, the conversation is processed on your computer and is not sent anywhere.

### Hugging Face (model downloads)

When you search for a model or download one, the app connects directly to Hugging Face (`huggingface.co`). Hugging Face sees your IP address, your search text, the files you ask for, and standard request headers, including the app's name and version. No Demios account is needed. Before you download, the app shows the model's licence and a link to its model card. Your chats are never sent to Hugging Face. [Hugging Face privacy policy](https://huggingface.co/privacy).

Some models need your Hugging Face token. If you save one, it is kept in your system keychain and sent only to Hugging Face. If you set a mirror with `HF_ENDPOINT`, the app and the `dn` tool use it instead, and the token goes to the mirror only if you opt in.

### Updates (copies downloaded from GitHub)

A copy of the app downloaded from GitHub can update itself. It checks for a new version only when you select **Check for updates** in **Settings → General → About**. The app then asks GitHub (`github.com`, which sends it on to `release-assets.githubusercontent.com`) for a small file that names the newest version. The address is the same for everyone and carries nothing about you or your copy. GitHub sees your IP address and standard request headers. If a newer version exists, the app shows its release notes, and downloads it from GitHub only after you select **Install and restart**. Nothing else is sent. Demios isn't offered in any app store yet. If it is later, store copies will be updated by their store and won't contact GitHub for updates. [GitHub privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

### Tool servers (MCP)

Tool servers let the model use tools. You add them in **Settings → Models → Tool servers**. There are two kinds:

- **Local servers** are programs on your computer that the app starts for you. Before a program starts for the first time, and again whenever its command changes, the app asks you in a system dialog that shows the exact command. A local server runs with your permissions: it can read and change your files and reach the internet on its own, according to what the program does. Only add servers you trust.
- **Remote servers** are online services. The app connects to them over HTTPS. If a server needs you to sign in, **Sign in** opens its sign-in page in your browser, and the server sees the app as the client "Demios Desktop". **Sign out** deletes the stored sign-in.

When the model calls a tool, the server receives the **arguments of that call**. These can contain parts of your conversation. The tool's answer comes back into the conversation, so it is also sent to your AI provider if you use one.

Tools that change something ask for your approval first: every time, or, for tools that only change local data, once per chat if you choose. Tools a server marks as read-only run **without asking**, so their arguments are sent without a prompt. A remote server is never trusted to mark its own tools read-only, so none of its tools run without asking. Each server's own policy applies.

### Artifacts

Artifacts run in a sealed-off frame with no network access. If an artifact tries to use the network, you can allow it for that one artifact after a confirmation that explains the risk; it can then send what it shows, including text from the chat, to websites it chooses. The permission is turned off again when the model writes a new version.

### Links in answers

The app never opens a link on its own. When you select a link in an answer, a system dialog shows the full address, and the link opens in your web browser only if you select **Open in browser**. The website's own privacy policy then applies.

### Reporting a problem

**Report a problem** and **Report this answer** open a form on GitHub in your browser. The form is prefilled with the app version, your operating system and, for an answer, the model's identifier. No chat content is included; you decide what to write. What you send is public on GitHub and is handled under [GitHub's privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

## Children

The app is not meant for children under 13 (or the minimum age in your country). We collect no data from anyone, including children. The AI model you choose can produce content that isn't suitable for children: the app doesn't filter it.

## Your choices and how to delete data

Because your data is on your own computer, you control it:

- **Delete a chat:** select the delete button next to it in the sidebar. You can undo this for 10 seconds. At the next start of the app, its messages, artifacts and tool history are removed for good. Only the chat's internal id and the time it was deleted are kept, with no title, model or content.
- **Remove a tool server:** go to **Settings → Models → Tool servers** and select the remove button next to it. After a confirmation, this deletes its settings, the secrets and sign-in saved for it in your keychain, and the "Allow in this chat" approvals for its tools. Chats that used its tools keep their history.
- **Remove an API key:** go to **Settings → Models → Provider keys** and select **Remove key**. The key is removed from the keychain straight away and held in memory for a few seconds, so you can undo.
- **Delete everything:** go to **Settings → General** and select **Delete all local data**. After a confirmation that lists what will go, it deletes the database (every chat, including recently deleted ones and their deletion records, every artifact, your settings and tool history), downloaded models, the keys, tokens and sign-ins Demios saved in your keychain (including your Hugging Face token), and the app window's own storage. This can't be undone.
- **Uninstalling** removes the app but not your data folder, your models or your keychain entries. Use **Delete all local data** first, or delete them yourself.
- **Data held by providers, Hugging Face, tool servers or GitHub** is controlled by them. Use their tools or contact them.

## Data protection (GDPR)

This section gives the information required by Articles 13 and 14 of the EU General Data Protection Regulation (GDPR).

### Controller

DigiNote Inc., at the address in [Who we are](#who-we-are). Email: mahdi@demios.io. Data protection officer: [DPO: to be confirmed by the owner whether one is required].

### What we process, why, and on what legal basis

| Situation | Data | Do we receive it? | Legal basis |
|---|---|---|---|
| Using the app: chats, artifacts, settings, tool history, models, keys | Everything listed in [What stays on your device](#what-stays-on-your-device) | **No.** It stays on your computer and we have no access to it. | None needed: we don't process it. |
| Chatting with a provider's model using your own key | Messages, tools, your key, your IP address | **No.** Your computer sends it straight to the provider you chose. | None needed: we don't process it. The provider is responsible for it. |
| Searching for and downloading models | Search text, requested files, your IP address | **No.** Your computer sends it straight to Hugging Face (or the mirror you set). | None needed: we don't process it. |
| Using tool servers | Tool arguments, which can contain chat text | **No.** Your computer sends it straight to the server you added. | None needed: we don't process it. |
| Reporting a problem on GitHub | What you write, the app version and your operating system | Only as a public GitHub issue, which we read as the project's maintainers. | Our legitimate interest in fixing problems in the app (Art. 6(1)(f) GDPR). |
| Emailing us | Your email address, what you write | **Yes.** | Answering you, and taking steps you ask for before a contract (Art. 6(1)(b)); otherwise our legitimate interest in answering questions about the app (Art. 6(1)(f)). |
| Checking for and installing updates (copies downloaded from GitHub) | Your IP address and standard request headers | **No.** Your computer asks GitHub directly, only when you select **Check for updates** or **Install and restart**. | None needed: we don't process it. |
| Visiting demios.io | Your IP address, the page requested, time, browser details | Our hosting provider receives them in its server logs. [Whether we can see these logs: to be confirmed by the owner] | Our legitimate interest in delivering the website securely (Art. 6(1)(f)). |

The website demios.io sets no cookies and uses no analytics or tracking. [To be confirmed by the owner for the published site.]

**Downloads:** the installers are served by GitHub, not by demios.io. When you download one, your browser connects to `github.com`, which sends it on to `release-assets.githubusercontent.com`. GitHub sees your IP address, the file you ask for, and standard request headers, under [GitHub's privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement). No GitHub account is needed.

The only script is on the download page. It reads which system your browser reports, to suggest the right installer, and sends nothing anywhere. The page lists every installer without it.

We don't sell personal data, use it for advertising or make automated decisions about you, including profiling.

You don't have to give us any personal data. The app works without contacting us at all.

### How long we keep it

- **Emails:** until your question or request is settled, then deleted, unless the law requires us to keep them longer (for example as business letters under German commercial or tax law). [Retention period: to be confirmed by the owner]
- **GitHub issues:** they stay public in the repository until you or we delete them. You can edit or delete your own issues and comments on GitHub.
- **Website server logs:** kept by the hosting provider under its own rules (see below).
- **Data on your computer:** until you delete it. See [Your choices and how to delete data](#your-choices-and-how-to-delete-data).

### Recipients and processors

- **Services you choose in the app** (AI providers, Hugging Face, tool servers) receive data directly from your computer, only when you set them up or start an action. They are not our processors. They act on their own responsibility, under your agreement with them.
- **GitHub** (GitHub, Inc., USA) hosts the project's repository and issues, and hosts the website demios.io on GitHub Pages. For the website, GitHub processes visitors' IP addresses and request details in its server logs. [Whether GitHub is a processor or a separate controller for this, and the agreement in place: to be confirmed by the owner]
- **GitHub (release downloads):** GitHub, Inc. serves the app's installers and updates from `github.com` and `release-assets.githubusercontent.com`, and processes downloaders' IP addresses and request details. [Whether GitHub is a processor or a separate controller for this, and the agreement in place: to be confirmed by the owner]
- **Our email provider** stores the emails you send us. [Email provider: to be confirmed by the owner]

### Transfers outside the EU

- **Services you choose:** Anthropic, OpenAI, Google and Hugging Face are based in or process data in the USA. Your computer sends data to them because you chose them; we don't transfer it.
- **GitHub:** GitHub, Inc. is in the USA. Transfers rely on the EU–US Data Privacy Framework, under the European Commission's adequacy decision of 10 July 2023, [GitHub's certification under the Data Privacy Framework: to be confirmed by the owner] and on the EU Standard Contractual Clauses in GitHub's data protection agreement.

### Your rights

You have the right:

- to know whether we process data about you, and to get a copy of it (Art. 15 GDPR),
- to have it corrected (Art. 16),
- to have it deleted (Art. 17),
- to have its processing restricted (Art. 18),
- to receive it in a common machine-readable format (Art. 20),
- **to object** at any time, for reasons arising from your particular situation, to processing based on our legitimate interest (Art. 21). We then stop, unless we have compelling legitimate grounds or need it for legal claims.

To use these rights, email mahdi@demios.io. Because the data in the app never reaches us, we can't see, copy or delete it for you; you can delete it yourself in the app. For data held by a service you chose, contact that service.

### Your right to complain

You can complain to a data protection supervisory authority, in particular in the EU country where you live or work, or where you think a breach happened (Art. 77 GDPR). The authority responsible for us is:

Landesbeauftragte für Datenschutz und Informationsfreiheit Nordrhein-Westfalen (LDI NRW)
Kavalleriestraße 2–4
40213 Düsseldorf
Germany
Website: [www.ldi.nrw.de](https://www.ldi.nrw.de)

## Changes to this policy

If the app starts sending any data to us (for example optional crash reports), we will update this policy **before** that version is released and describe the change in the release notes. Anything like that will be opt-in unless this policy says otherwise. The latest version is always at https://demios.io/privacy, and earlier versions are in the project's public repository.

## Contact

DigiNote Inc. · Niederkasseler Kirchweg 85, 40547 Düsseldorf, Germany · mahdi@demios.io · [GitHub issues](https://github.com/mimshavandi/demios-desktop/issues)

Legal notice (Impressum): https://demios.io/impressum [URL: to be confirmed by the owner]

For security problems, see [SECURITY.md](../../SECURITY.md).
