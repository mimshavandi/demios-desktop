# Adding a model

Demios Desktop doesn't come with a model. Add one in **Settings → Models**, in either of two ways:

- **On this device:** download an open model and run it on your computer. Nothing you type leaves your computer.
- **Your own key:** add an API key for Anthropic, OpenAI or Google Gemini. Your messages go to that provider under your account with them.

You can do both and switch between them.

## On this device

The **On this device** card lists 12 open models in a built-in catalogue, from small ones to large ones, including one embedding model.

Each row shows:

- The model's name, its size and quality version (quantisation, such as Q4_K_M) and its download size.
- **Whether it fits this computer's memory:**
  - **Ready**: it fits.
  - **Caution** or **Memory pressure**: it may run slowly, or slow down other apps.
  - **Won't fit**: it's likely too large for this computer's memory. The app never picks it on its own.
  - "(estimate)" after the status means the app couldn't measure everything and guessed.
- **Its licence and model card.** Select the copy button to copy the model card link and read it in your browser. If the licence is missing, non-commercial or custom, a warning asks you to check it before downloading.

To download a model, select **Download model**. While it downloads you see the progress and the time left. You can:

- Select **Pause**, and later **Resume**.
- Start it again after the app quit or the connection dropped: it continues from where it stopped instead of starting over.
- Select **Retry** if it failed.

When the download finishes, the app checks the file against its published checksum. Then the row shows **On this device** and the model can answer.

Models are stored in a folder shared with the `dn` command-line tool. **Settings → General → About** shows where.

## Your own key

Under **Provider keys** there's a field for each provider: Anthropic, OpenAI and Google Gemini.

1. Get an API key from the provider's website.
2. Paste it into the provider's field and select **Save key**.

The key is stored in your system keychain (Keychain on macOS, Credential Manager on Windows, Secret Service on Linux), not in a file. After saving, the app never shows the key again. To replace it, paste a new one. To remove it, select **Remove key**; you can undo that for 10 seconds.

Once a key is saved, that provider's models appear in the model lists:

| Provider | Models |
|---|---|
| Anthropic | claude-opus-5-5, claude-sonnet-5-5, claude-haiku-5-5 |
| OpenAI | gpt-5, gpt-5-mini |
| Google Gemini | gemini-2.5-pro, gemini-2.5-flash |

Using a provider's model costs whatever that provider charges your account.

## Choose which model answers

**Model for each task**, at the top of **Settings → Models**, sets which model does what. **Chat** is the model that answers in conversations.

If you don't choose a Chat model, the app picks one that is ready and says which: "Not set (uses … automatically)". Models on your computer are listed under **On this device**, the others under their provider.

The **Fast** and **Embeddings** rows are for features that aren't in the app yet. You can leave them unset.
