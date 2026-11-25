# Projects

A project keeps related chats together, with files and instructions that apply to every chat in it.

## Create a project

1. In the sidebar, select the **New project** button next to **Projects**.
2. Type a name and select **Create project**.

The project opens on its own page. Select it under **Projects** in the sidebar to come back to it.

## Instructions

Under **Details**, write what every chat in the project should follow, for example "Answer in British English" or "The reader is a first-year student". Select **Save changes**. Instructions can be up to 8,000 characters.

The instructions are sent to the model with each message in the project's chats, so a model from a provider receives them too. Chats outside the project don't get them.

If you leave the page before saving, your changes are kept until you come back. "Unsaved changes" next to the button reminds you.

## Files

Select **Add files** and pick one or more files. Demios keeps a copy of each file in its data folder, so moving, changing or deleting the original afterwards doesn't affect the project. A file can be up to 50 MB, and a project can hold up to 100 files. Files that can't be added are listed with the reason.

To remove a file, select the remove button next to it. A message lets you **Undo** for 10 seconds. The copy is deleted for good the next time the app starts.

## Answers from your files

In a project's chats, Demios searches the project's text files for each question and gives the model the best passages, so answers can use them. Only text files of up to 2 MB are searched. Chats outside a project don't use any files.

Under each answer, a list shows the sources the model was given, for example "[S1] plan.md lines 3–9". If nothing matched, it says "No passages in this project's files matched this question." Sources record only the file and lines, not the text.

The passages are sent to the model with your question. With a model on this device, nothing leaves your computer. With a provider's model, the passages go to that provider. See [Privacy and your data](privacy.md).

### Better search

At first, files are searched by keywords, and the sources under an answer say "Keyword search". To also find passages by meaning, download the search model (about 84 MB):

- The first time a project has files, its page asks **Turn on better search?** Choose **Download** or **Not now**. After **Not now**, it doesn't ask again.
- You can download it any time from **Project search** in **Settings → Models**.

While the model downloads and your files are prepared, Sources show the progress, and keyword search keeps working.


## Chats in a project

On the project's page, select **New chat**. The title bar shows the project's name before the chat's title. Like any new chat, it's saved when you send the first message. The project's chats are listed on its page and also under **Recent** in the sidebar.

## Move a chat into or out of a project

Under the message box of any chat, the **Project** list shows which project the chat is in. Choose another project to move the chat there, or **No project** to take it out. A message lets you **Undo** for 10 seconds. The list is shown once you have a project.

From the next message on, the chat follows the instructions of the project it's in now. Answers it already gave stay as they are. A chat you moved out of a project stays when that project is deleted.

For a new chat, choosing a project before the first message starts the chat in that project.

## Delete a project

At the bottom of the project's page, select **Delete project**. Its chats and files are deleted with it. A message lets you **Undo** for 10 seconds; until the app restarts, Undo brings back the project, its chats and its files. After that, they're removed for good.

**Delete all local data** removes every project too. See [Privacy and your data](privacy.md).
