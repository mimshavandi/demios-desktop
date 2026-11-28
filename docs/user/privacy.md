# Privacy and your data

Demios collects no usage data. There is no Demios account and no Demios server.

**Settings → General → Privacy** shows the same information as this page, with the folder your chats are stored in.

## Where your data goes

- **On this computer.** Chats, projects, artifacts and settings are stored in a database in the app's data folder. Files you add to a project are copied into the same folder. Provider keys are kept in your system keychain. Models on this device answer without sending anything off your computer.
- **Model providers you add a key for.** Messages in chats that use a provider's model are sent to that provider under your account with them, and their terms apply. In a project's chats, the project's instructions are sent with each message too. With memory on, what Demios remembers about you that applies to the chat is sent with each message, and the provider is asked to suggest memories from your own messages in that chat. Mail, web pages and files other people wrote are only read for memory by a model on this computer, never sent to a provider for it.
- **Hugging Face.** Searching for and downloading a model contacts Hugging Face. If you save a Hugging Face token for gated models, it's kept in your keychain and sent only to Hugging Face. Your chats are never sent there.
- **Your Demios Node, if you pair one.** Your chats and projects sync to a Node you run on your own machine, and through it to your other paired devices, encrypted end to end. Nothing goes to Demios. See [Your Demios Node](node.md).
- **Tool servers (MCP).** A tool server receives what its tools are asked to do, which can include text from your chat. See [Tools and approvals](tools-and-approvals.md).
- **GitHub, for updates.** If you downloaded Demios from GitHub, **Settings → General → About → Check for updates** asks GitHub for the newest version when you select it. If you turn on **Check for updates when Demios starts** (it's off unless you do), Demios also asks once each time it starts. **Install and restart** downloads the update from GitHub after you confirm. Nothing else is sent. Demios isn't in any app store yet.

**Settings → General → About** shows the exact folders for your chats and for downloaded models. The confirmation for **Delete all local data** shows them too.

The full privacy policy is at [demios.io/privacy](https://demios.io/privacy), and the accessibility statement at [demios.io/accessibility](https://demios.io/accessibility). **Settings → General → About** links to both. Demios asks before it opens your browser.

## Memory

Memory is a list of short facts Demios keeps about you, in **Settings → Memory**. You decide what's on it:

- Things you tell Demios to remember are saved at once. Anything Demios suggests waits for you to keep it, unless you turn on **Remember without asking**. Anything learned from someone else's mail, web pages or files always waits, and is saved only as what they said.
- Demios never saves health details, money matters, ID numbers or passwords.
- Each memory shows where it came from and which model suggested it. Deleting the chat a memory came from deletes the memory too, unless you've kept it from another source.
- **Use memory** off stops chats from reading or adding memories; the list stays until you delete it. A project can keep its own memories apart from the rest.
- **Incognito chats** don't read or add memories, aren't found when Demios searches your past chats, aren't synced, and are deleted when you close them. A provider still gets the messages, as in any chat with it.
- **Delete all memories** in Settings → Memory deletes the list and the suggestions waiting. **Export memories** saves them to a file, which isn't encrypted.

## Delete one chat

In the sidebar, open the chat's **More actions** menu (the … button) and choose **Delete**. You can undo it for 10 seconds. After that, it's removed for good, with its artifacts and any memory that came only from it, the next time the app starts. See [Chats](chats.md).

## Delete a project or a project file

On the project's page, **Delete project** deletes it with its chats and files, and the remove button next to a file removes that file. Both can be undone for 10 seconds and are removed for good, including the file copies, the next time the app starts. See [Projects](projects.md).

## Delete all local data

**Settings → General → Delete all local data** removes everything Demios stored on this computer:

- The database: every chat (including recently deleted ones) and its artifacts, every project with its instructions, your memories and suggestions, your settings and tool history.
- The copies of files you added to projects, and the search index made from them.
- Downloaded models, including the ones the `dn` command-line tool uses. Models in your own Hugging Face cache are left alone.
- The keys and tokens Demios saved in your system keychain, including your Hugging Face token.
- The app window's own storage.
- If this computer is paired with a Node: sync ends here, and this computer's device keys are deleted. Nothing is deleted on your other devices, and the Node still lists this computer until you run `dn node remove-device` on the Node, or remove it from another paired device. See [Your Demios Node](node.md).

Before anything is deleted, a confirmation lists what will go, with the number of chats, the size of your models and the keys found. **This can't be undone.** Your accounts with providers aren't affected; to close those, go to the provider.

Your own skills folder is left alone, because you wrote those files. See [Skills](skills.md).

After deleting, the app starts over as if it were new, including the first-run notice.
