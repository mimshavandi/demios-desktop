# Privacy and your data

Demios collects no usage data. There is no Demios account and no Demios server.

**Settings → General → Privacy** shows the same information as this page, with the folder your chats are stored in.

## Where your data goes

- **On this computer.** Chats, artifacts and settings are stored in a database in the app's data folder. Provider keys are kept in your system keychain. Models on this device answer without sending anything off your computer.
- **Model providers you add a key for.** Messages in chats that use a provider's model are sent to that provider under your account with them, and their terms apply.
- **Hugging Face.** Searching for and downloading a model contacts Hugging Face. If you save a Hugging Face token for gated models, it's kept in your keychain and sent only to Hugging Face. Your chats are never sent there.
- **Tool servers (MCP).** A tool server receives what its tools are asked to do, which can include text from your chat. See [Tools and approvals](tools-and-approvals.md).
- **GitHub, for updates.** If you downloaded Demios from GitHub, **Settings → General → About → Check for updates** asks GitHub for the newest version when you select it. If you turn on **Check for updates when Demios starts** (it's off unless you do), Demios also asks once each time it starts. **Install and restart** downloads the update from GitHub after you confirm. Nothing else is sent. Copies from the Microsoft Store or Flathub are updated by the store and never contact GitHub.

**Settings → General → About** shows the exact folders for your chats and for downloaded models. The confirmation for **Delete all local data** shows them too.

The full privacy policy is at [demios.io/privacy](https://demios.io/privacy), and the accessibility statement at [demios.io/accessibility](https://demios.io/accessibility). **Settings → General → About** links to both. Demios asks before it opens your browser.

## Delete one chat

Point at the chat in the sidebar and select the delete button. You can undo it for 10 seconds. After that, it's removed for good, with its artifacts, the next time the app starts. See [Chats](chats.md).

## Delete all local data

**Settings → General → Delete all local data** removes everything Demios stored on this computer:

- The database: every chat (including recently deleted ones) and its artifacts, your settings and tool history.
- Downloaded models, including the ones the `dn` command-line tool uses. Models in your own Hugging Face cache are left alone.
- The keys and tokens Demios saved in your system keychain, including your Hugging Face token.
- The app window's own storage.

Before anything is deleted, a confirmation lists what will go, with the number of chats, the size of your models and the keys found. **This can't be undone.** Your accounts with providers aren't affected; to close those, go to the provider.

After deleting, the app starts over as if it were new, including the first-run notice.
