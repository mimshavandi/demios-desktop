# Adding a model

Demios Desktop doesn't come with a model. Add one in **Settings → Models**, in either of two ways:

- **On this device:** download an open model and run it on your computer. Nothing you type leaves your computer.
- **Your own key:** add an API key for Anthropic, OpenAI or Google Gemini. Your messages go to that provider under your account with them.

You can do both and switch between them.

## On this device

The **On this device** card lists 13 open models in a built-in catalogue, from small ones to large ones: 11 for chat and 2 embedding models.

Each row shows:

- The model's name, its size and quality version (quantisation, such as Q4_K_M) and its download size.
- **Whether it fits this computer's memory:**
  - **Ready**: it fits.
  - **Caution** or **Memory pressure**: it may run slowly, or slow down other apps.
  - **Won't fit**: it's likely too large for this computer's memory. The app never picks it on its own.
  - "(estimate)" after the status means the app couldn't measure everything and guessed.
  - "To fit, part of this model stays in regular memory" means a large mixture-of-experts model (such as a 30B model with about 3B active parameters) keeps its experts in regular memory and the rest on the graphics chip. It fits on a computer with 16–32 GB of memory but replies a little slower. "Part of this model runs on the processor" means the same for other models, which slows replies more.
- **Its licence and model card.** Select the copy button to copy the model card link and read it in your browser. If the licence is missing, non-commercial or custom, a warning asks you to check it before downloading.

To download a model, select **Download model**. While it downloads you see the progress and the time left. You can:

- Select **Pause**, and later **Resume**.
- Start it again after the app quit or the connection dropped: it continues from where it stopped instead of starting over.
- Select **Retry** if it failed.

When the download finishes, the app checks the file against its published checksum. Then the row shows **On this device** and the model can answer.

Models are stored in a folder shared with the `dn` command-line tool. **Settings → General → About** shows where.

## Add a model from Hugging Face

Beyond the catalogue, you can add GGUF models from Hugging Face that run on this computer. A model needs a published checksum to be downloaded, and some need your Hugging Face token (see below). In the **On this device** card, select **Add from Hugging Face**.

1. **Search.** Type a name, such as "qwen 1.5b instruct", and select **Search**. Sort by most downloaded, most liked, trending or recently updated, and select **Show more** for more results. Searching contacts Hugging Face; your chats are never sent there.
2. **Pick a model.** While the app reads the model's files, it says "Checking model…". Nothing is downloaded yet. Then you see:
   - **Its licence and model card.** A note tells you when the licence is missing, doesn't allow commercial use, is the model's own, or comes with an acceptable-use policy. Read it on the model card before downloading.
   - **Its versions**, each a different size and quality (quantisation), with whether it fits this computer's memory. "(estimate)" means the fit was worked out from another version of the same model. "Fit unknown" means its details couldn't be read.
3. **Download.** Select **Download** next to a version. You see the progress and can **Pause** and **Resume**, or **Retry** if it failed. The file is checked against the checksum Hugging Face publishes. A version without a published checksum can't be downloaded. When it's done, the version shows **Downloaded** and the model can answer.

Select **Done** to go back to the list. Models you add this way show "From Hugging Face" on their row.

### Links that add a model

A web page or a message can contain a `demios://models/add` link that points at a model on Hugging Face. Opening it brings Demios to the front on **Settings → Models → On this device**, with a note that a link asked to add this model. You see the same details as above: the licence, the model card and whether each version fits. The version the link chose is listed first and marked **Chosen by the link**.

Nothing downloads until you select **Download**. Select **Not now** to close it. While one link's model is open, other links are ignored.

### Models that need access

Some models are gated: you have to accept their terms first. For those, the app asks for a Hugging Face token:

1. Accept the model's terms on its model card.
2. Create a token with read access in your Hugging Face account settings.
3. Paste it into **Hugging Face token** and select **Save token**.

The token is kept in your system keychain and is sent only to Hugging Face. Select **Remove token** to delete it.

### Models already on your computer

If you've used Hugging Face tools before, the app also lists GGUF models it finds in your Hugging Face cache. Their row says "From your Hugging Face cache, used where it is". The app uses the file in place and never changes or deletes it, not even with **Delete all local data**.

Models you downloaded from Hugging Face with the `dn` command-line tool show up here too.

### Chat templates

Each model on this device writes its prompts with its own chat template. The template comes from the model file. If the file has none, Demios uses the template from the model's Hugging Face repository, saved when you downloaded the model. A model without a chat template can't be used for chat: its row says "This model has no chat template, so it can't be used for chat." Choose another model, or another version of the same one.

## Your own key

On the **Providers** tab there's a field for each provider: Anthropic, OpenAI and Google Gemini.

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
