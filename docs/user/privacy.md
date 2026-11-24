# Privacy and your data

Demios collects no usage data. There is no Demios account and no Demios server.

**Settings → General → Privacy** shows the same information as this page, with the folders on your computer.

## Where your data goes

- **On this computer.** Chats and settings are stored in a database in the app's data folder. Provider keys are kept in your system keychain. Models on this device answer without sending anything off your computer.
- **Model providers you add a key for.** Messages in chats that use a provider's model are sent to that provider under your account with them, and their terms apply.
- **Hugging Face.** Downloading a model fetches it from Hugging Face. Your chats are never sent there.
- **Tool servers (MCP).** A tool server receives what its tools are asked to do, which can include text from your chat. Adding tool servers isn't in the app yet; see [Tools and approvals](tools-and-approvals.md).

**Settings → General → About** shows the exact folders for your chats and for downloaded models.

## Delete one chat

Point at the chat in the sidebar and select the delete button. You can undo it for 10 seconds. After that, it's removed for good the next time the app starts. See [Chats](chats.md).

## Delete all local data

**Settings → General → Delete all local data** removes everything Demios stored on this computer:

- The database: every chat (including recently deleted ones), your settings and tool history.
- Downloaded models, including the ones the `dn` command-line tool uses.
- The keys Demios saved in your system keychain.
- The app window's own storage.

Before anything is deleted, a confirmation lists what will go, with the number of chats, the size of your models and the keys found. **This can't be undone.** Your accounts with providers aren't affected; to close those, go to the provider.

After deleting, the app starts over as if it were new, including the first-run notice.
