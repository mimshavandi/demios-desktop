# Tools and approvals

Tools let a model do things beyond writing an answer, like reading a file or calling a service. They come from tool servers that use the Model Context Protocol (MCP).

**Not in the app yet:** you can't add a tool server from the app. Until a screen for that arrives, chats don't use tools. This page describes how approvals work, so you know what to expect.

## What asks first

Each tool is put in a risk group from what its server says about it. A server the app reaches over HTTP isn't trusted to call its own tools harmless, so its tools never go below "Changes local data".

| Badge | Meaning | Asks first? |
|---|---|---|
| Reads data | Only reads | No |
| Changes local data | Changes something on this computer | Yes; you can allow it for the whole chat |
| Acts outside the app | Sends or changes something elsewhere | Yes, every time |
| Can delete or overwrite | Can destroy data | Yes, every time |
| Sensitive | Touches sensitive data | Yes, every time |

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
