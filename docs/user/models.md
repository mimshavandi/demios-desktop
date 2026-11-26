# Adding a model

Demios Desktop doesn't come with a model. Add one in **Settings → Models**, in either of two ways:

- **On this device:** download an open model and run it on your computer. Nothing you type leaves your computer.
- **Your own key:** add an API key for Anthropic, OpenAI or Google Gemini. Your messages go to that provider under your account with them.

You can do both and switch between them.

## On this device

The **On this device** card lists 14 open models in a built-in catalogue, from small ones to large ones: 11 for chat and 3 embedding models.

Under its title, a line says where local models run on this computer:

- **On the graphics card**, with its name and memory. On Windows and Linux this works with NVIDIA, AMD and Intel graphics that have a current driver.
- **On the built-in graphics**, which share the computer's memory (Apple Silicon Macs, laptops with integrated graphics).
- **On the processor**, when there is no graphics chip Demios can use (for example in a virtual machine, or without a graphics driver). Models still work, but replies are slower; smaller models are a good fit.

Whether a model fits (below) is worked out for that graphics card or memory.

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
- Select **Cancel download** to stop it and delete the part already downloaded. A paused download can be cancelled too.
- Start it again after the app quit or the connection dropped: it continues from where it stopped instead of starting over.
- Select **Retry** if it failed.

When the download finishes, the app checks the file against its published checksum. Then the row shows **On this device** and the model can answer. If a model file is in the models folder but doesn't match its published checksum, the row says so, with **Retry** to download it again; the file isn't used.

To free the space a downloaded model takes, select **Remove** on its row. It's removed at once: it's no longer offered in the menus, and a task it was the model for (such as **Chat**) is emptied, so that task asks you to choose a model instead of using another one on its own. You can undo it for 10 seconds, from the message or from the row; the file is deleted after that. The app never deletes models in your Hugging Face cache or in a models folder you chose: there **Remove** is turned off, and the row says why.

Models are stored in a folder shared with the `dn` command-line tool. **Settings → General → About** shows where.

## Add a model from Hugging Face

Beyond the catalogue, you can add GGUF models from Hugging Face that run on this computer. A model needs a published checksum to be downloaded, and some need your Hugging Face token (see below). On **Settings → Models → On this device**, select **Add a model**. A window opens over Settings (it fills the window when the app is narrow). **Esc** or the close button closes it, and your search stays for next time.

1. **Search.** Type a name, such as "qwen 1.5b instruct", and select **Search**. Sort by most downloaded, most liked, trending or recently updated, and select **Show more** for more results. Searching contacts Hugging Face; your chats are never sent there.
2. **Pick a model.** While the app reads the model's files, it says "Checking model…". Nothing is downloaded yet. Then you see:
   - **Its licence and model card.** A note tells you when the licence is missing, doesn't allow commercial use, is the model's own, or comes with an acceptable-use policy. Read it on the model card before downloading.
   - **Its versions**, each a different size and quality (quantisation), with whether it fits this computer's memory. "(estimate)" means the fit was worked out from another version of the same model. "Fit unknown" means its details couldn't be read.
3. **Download.** Select **Download** next to a version. The window closes, and the download shows in the **On this device** list with its progress. There you can **Pause** and **Resume**, or **Retry** if it failed. It keeps going if you leave Settings. The file is checked against the checksum Hugging Face publishes. A version without a published checksum can't be downloaded. When it's done, a message says so and the model can answer.

Models you add this way show "From Hugging Face" on their row.

### Links that add a model

A web page or a message can contain a `demios://models/add` link that points at a model on Hugging Face. Opening it brings Demios to the front on **Settings → Models → On this device** and opens the **Add a model** window on that model, with a note that a link asked to add this model. You see the same details as above: the licence, the model card and whether each version fits. The version the link chose is listed first and marked **Chosen by the link**.

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

If the app can't read your system keychain (none is installed, it is locked, or access was refused), Settings › Models says so above the keys, with what to do and a **Retry** button, instead of showing "No key yet". If the keychain asks for a password, answer it; the list updates once it has.

Once a key is saved, that provider's models appear in the model lists:

| Provider | Models |
|---|---|
| Anthropic | claude-opus-5-5, claude-sonnet-5-5, claude-haiku-5-5 |
| OpenAI | gpt-5, gpt-5-mini |
| Google Gemini | gemini-2.5-pro, gemini-2.5-flash |

Using a provider's model costs whatever that provider charges your account.

## A model server (Ollama, LM Studio and others)

If you already run models in Ollama, LM Studio, llama.cpp's `llama-server`, vLLM, or any server with an OpenAI-compatible API, add it in Settings › Models › **Model servers**:

1. Select **Add model server** and enter its address, for example `http://localhost:11434` (Ollama) or `http://localhost:1234` (LM Studio). If the address has no path, `/v1` is added.
2. Optionally give it a name, and an API key if the server needs one. The key is stored in your system keychain and sent only to this server. On Linux, saving a key needs a system keychain (GNOME Keyring or KWallet); a server without a key works without one.
3. Select **Add**. The app lists the server's models; they appear in the model lists under the server's name.

Each server shows whether it answers: **Connected**, **Needs key** (the server wants a key, or refused the one you saved), or **Unreachable**, with **Retry**.

Addresses on your computer or local network (for example `localhost`, `192.168.x.x`, `10.x.x.x` or a `.local` name) can use `http://`. Any other server needs `https://`. The app never follows a redirect to another address.

Messages in chats that use a server's model go to that server.

**Tool calls.** Making artifacts and using tools need a model that makes tool calls. Ollama, LM Studio and llama.cpp say which of their models do; the list shows **Tool calls** or **No tool calls** for each. If a server doesn't say, turn **tool calls** on for a model only if it supports them. Otherwise the model can chat, but can't make artifacts or use tools.

Select **Refresh** after loading or removing models on the server. **Remove** deletes the server's address, model list and key from this computer, and tasks that used one of its models ask you to choose another. You can undo it for 10 seconds.

## Choose which model answers

**Model for each task**, at the top of **Settings → Models**, sets which model does what. **Chat** is the model that answers in conversations.

Models on your computer are listed under **On this device**, the others under their provider.

**Your first model is saved as the choice.** When a task has no model and you download or add the first model that suits it (a chat model for **Chat**, a search model for **Project search**), Demios saves that model as the task's choice. Downloading or adding more models later doesn't change it; you change it in the menu, or with **Set as default** (below).

If a task has no model and you have several that suit it (for example after updating from an earlier version), Demios doesn't pick one for you. The task says so, for example "Choose a model for Chat." under the message box, with **Choose a model**, which opens this page. If you clear a task's menu, it stays empty until you choose again.

**Set as default:** each model you can use, under **On this device**, **Model servers** and **Providers** (once its key is saved), has **Set as default** for the task it suits. Selecting it makes that model the task's choice and says so ("Now used for Chat"); the row then shows **Default for Chat**. A search model shows it only if it runs on this device, since project search uses models on this device.

**Project search** is the model that finds passages in your project files by meaning (see [Projects](projects.md)). It's chosen like **Chat**: pick a downloaded embedding model in the menu, or select **Set as default** on its row. While none is chosen, the row says "No model chosen: project files are searched by keywords." A search model's download, its progress and any problem show on its own row under **On this device**, like any other model.

**Recommended:** models we recommend have a **Recommended** chip, in the **On this device** list and in **Add a model** results:

- **Project search:** Nomic Embed Text v2 MoE (Q5_K_M), on every computer.
- **Chat:** one model for your computer's memory. With 8 GB up to 15 GB, that's Llama 3.2 3B Instruct; with 16 GB or more, Qwen3 8B. Computers with less than 8 GB get no chat recommendation. You only ever see one chat model marked **Recommended**.
