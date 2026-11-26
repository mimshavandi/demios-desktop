# Tools and approvals

Tools let a model do things beyond writing an answer, like reading a file or calling a service. They come from tool servers that use the Model Context Protocol (MCP).

## Add a tool server

Go to **Settings → Tools** and select **Add tool server**. Give it a name; the ID is suggested from the name and can't change later. Then choose where it runs:

- **On this computer (a program Demios starts):** enter the command, its arguments and any environment variables. The program runs with your permissions, so add only programs you trust.
- **Remote (an HTTPS address):** enter the server address and any headers. If the server needs you to sign in with an account, turn on **Sign in with the server's account (OAuth)**.

Mark a variable or header as secret, such as an API token, and its value goes into your system keychain. It is never shown again; leave a saved one empty to keep it. Names that look like secrets are marked secret for you.

Select **Add tool server** to save. The server connects, and its row shows **Connected** with the number of tools, or why it failed.

### Starting a local server

Before Demios starts a program for the first time, a system dialog, **Start a tool server?**, shows the exact command, every argument and the environment variables (secret values are hidden). Select **Allow and start** or **Don't start**. Your answer is remembered for that exact command; if the command changes, you're asked again.

A Flatpak build of Demios can't start programs on your computer and offers remote servers only. There is no published Flatpak yet.

### Signing in to a remote server

For a server that uses sign-in, select **Sign in** on its row. The server's sign-in page opens in your browser; finish there, and the row changes to **Connected**. **Sign out**, in the row's **More actions** menu, deletes the stored sign-in and keeps the server's other settings.

### Change or remove a server

Each server's row has a **More actions** menu (the … button):

- **Edit** changes a server's settings. If its address or program changes, type its secrets again.
- **Reconnect** tries again after a server failed.
- **Remove** stops the server and deletes its settings, the secrets and sign-in saved for it in your keychain, its tool choices, and the "Always allow in this chat" approvals for its tools. For 10 seconds, **Undo** on the message that appears puts it all back and connects the server again. Chats that used its tools keep their history.

## What asks first

Each tool is put in a risk group from what its server says about it. A remote server isn't trusted to call its own tools harmless, so its tools never go below "Changes local data".

| Badge | Meaning | Asks first? |
|---|---|---|
| Reads data | Only reads | No |
| Changes local data | Changes something on this computer | Yes; you can allow it for the whole chat |
| Acts outside the app | Sends or changes something elsewhere | Yes, every time |
| Can delete or overwrite | Can destroy data | Yes, every time |
| Sensitive | Touches sensitive data | Yes, every time |

## See and change each tool

Select **Tools** on a server's row to list its tools. Each shows:

- its name and the description the server gives it. Descriptions are shown as plain text, exactly as the model reads them; a server can use them to slip instructions to the model, so read them.
- **Server says:** the risk group from the server's own description of the tool.
- **Now:** whether it runs without asking, asks once per chat, asks every time, or is off.

Under **When a model uses it**, choose **Ask every time**, **Ask once per chat**, **Run without asking** or **Off**, or **Follow its risk tier** to go back to the default. Your choice is kept and applied every time the server connects.

Some choices ask you to confirm first, because a model could then do real harm without showing you:

- **Run without asking** for a tool that acts outside the app, can delete or overwrite, or is sensitive.
- **Ask once per chat** for a tool that can delete or overwrite, or is sensitive.

The confirmation names the tool and what it could do. **Keep asking** leaves your choice as it was; select the choice's own button (**Run without asking** or **Ask once per chat**) to apply it. Tools that only read or change local data don't ask for a confirmation.

### New or changed tools

Demios remembers each server's tools as they were when it first connected. If a server later adds a tool, or changes one's description, inputs or risk hints, the tool is marked **New** or **Changed**, the row shows how many tools are **to review**, and those tools ask before every use, whatever your choice. Read what changed, then select **Mark reviewed** (or **Mark all reviewed**).

## The approval card

When a tool needs your OK, a card appears in the chat. It shows what the tool will do, its risk badge, and the exact arguments it will receive. Then choose:

- **Allow once** (⌘↵, Ctrl+Enter on Windows and Linux): run it this time.
- **Always allow in this chat**: don't ask again for this tool in this chat. It's offered only for tools that change local data.
- **Deny** (Esc): don't run it.

Nothing runs without an answer. If you don't answer within 10 minutes, the request expires and the tool doesn't run. If a different chat is waiting for your approval, a message at the top offers **Open chat**.

## "Did this go through?"

If the app quit or crashed while a tool that changes something was running, the app can't know whether it finished. A card asks you to check, then choose **It went through** or **It didn't happen**. The app won't run that call again until you answer.

## Where tool data goes

A tool server receives what its tools are asked to do, which can include text from your chat. See [Privacy and your data](privacy.md).
