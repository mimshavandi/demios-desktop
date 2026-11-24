# Artifacts

When you ask for something you'd keep or use, like a web page, a small app, a diagram or a document, the model can make it as an **artifact**. Artifacts open in a pane next to the chat, where you can see them, run them and go back to earlier versions.

**Windows:** artifacts aren't available on Windows yet. They're still saved, and will open in a later version. Everything else on this page applies to macOS and Linux.

## Kinds of artifact

| Kind | What you see |
|---|---|
| HTML | A web page or small app, running in the pane |
| React | A small interactive app |
| SVG | An image |
| Diagram | A diagram drawn from a text description (Mermaid) |
| Document | Formatted text (Markdown) |
| Code | Source code |

## The artifact pane

When the model makes or changes an artifact, a card appears in its answer with the artifact's title, kind and version, for example "HTML · v2". The pane opens on its own. To open an artifact later, select **Open** on its card. If the chat has several artifacts, the pane lists them.

- **Esc** closes the pane.
- The button at the top right of the window shows or hides the pane.

Each artifact runs in a sealed-off frame: it can't reach your files, the rest of the app or the internet, unless you allow network access for it (see below).

## Versions

Each time the model changes an artifact, it becomes a new version. Use the version picker, or **Previous version** and **Next version**, to look at earlier ones. When you're looking at an older version, the pane says which version is the latest.

To go back to an older version, select **Restore this version**. This doesn't delete anything: it adds a new version that is a copy of the old one, labelled for example "v3 — restored from v1 · you".

## Network access

Artifacts can't use the network. If an artifact tries to, the pane says so and offers **Allow network for this artifact…**. Selecting it asks you to confirm, and explains the risk: the artifact could send anything it shows, including text from this chat, to any website. Choose **Allow network** or **Keep it off**. **Allow network** stays disabled for a moment so a double-click can't confirm it by accident.

The permission is for that one artifact only. Plain http and other artifacts stay blocked. When the model writes a new version of the artifact, network access is turned off again and you have to allow it again.

## When an artifact has problems

If an artifact's code reports errors while it runs, an error bar shows the first problem, with **Details** for the rest. Select **Ask to fix** to send the errors to the model and ask it for a corrected version. The error text is passed on as data from the artifact, and the model is told not to follow instructions in it.

## AI label

A version the model made is labelled **AI-generated · Made by** and the model's name. A version you restored is labelled as yours. When an artifact is shown, it also carries a machine-readable "AI-generated" marker.

## Limits

A chat can have up to 100 artifacts, and each artifact up to 200 versions. Past those limits the model can't add more.

## Where artifacts are stored

Artifacts are saved with your chats, in the app's database on your computer. Deleting a chat deletes its artifacts when it's removed for good (at the next start). **Delete all local data** deletes every artifact. See [Privacy and your data](privacy.md).
