# Skills

A skill changes how the assistant works in one chat. It adds instructions for that chat, such as how to structure a summary or a review, and it can limit which tools the model may use.

## Choose a skill

In the message box, open the list that shows **No skill** and choose one. The skill applies from the next answer, and the chat keeps it until you change it. In a new chat, the skill you choose is saved when you send the first message.

A skill never skips a tool approval. It can only narrow the tools a model may use, never add tools or let one run without asking. See [Tools and approvals](tools-and-approvals.md).

## Starter skills

| Skill | What it does | Tools it allows |
|---|---|---|
| Compare | Compares two or more options, products or texts side by side and says where they differ | None |
| Critique | Gives honest, specific feedback on a piece of writing or a design you share, with concrete fixes | None |
| Design review | Reviews a technical proposal or system design for risks, trade-offs and open questions | Artifacts |
| Outline | Turns a topic or rough notes into a structured outline for a presentation, talk or document | Artifacts |
| Summarize | Summarises text you paste (an article, a thread, notes) into the key points and any actions | None |
| Write email | Drafts a new email or a reply to one you paste, in a tone you can send as is | None |

## Your own skills

You can write your own skills. Each one is a folder with a `SKILL.md` file: a short header with the skill's `name`, a one-line `description` and the tools it may use (`allowed-tools`), followed by the instructions in Markdown. The folder name and the `name` must match. The [skills README](../../skills/README.md) shows the format and the starter skills as examples.

1. Go to **Settings → General → Skills** and select **Open skills folder**.
2. Add your skill's folder there.
3. Select **Reload**.

Your skills appear in the list marked **Yours**, next to the **Built in** ones, and in the message box's skill list. Your own skills follow the same rules: they can only narrow tools and never skip an approval. If a file has a mistake, the Skills card lists it with the problem, and that skill isn't offered until you fix it and select **Reload**. A skill can't use the name of a built-in one.

**Delete all local data** leaves your skills folder alone, because you wrote those files.

## Where your choice is stored

The skill chosen for each chat is saved with your chats on this computer. A skill's instructions are sent to the model with each message in that chat, so a model from a provider receives them too. **Delete all local data** removes every choice. See [Privacy and your data](privacy.md).
