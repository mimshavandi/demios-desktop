# The dn command-line tool

`dn` runs and manages models from the terminal. It uses the same model folder as the app, so models you download with `dn`, from the catalogue or from Hugging Face, show up in the app too.

## Install

From the repository root:

```sh
cargo install --path apps/cli
```

Or run it without installing: `cargo run -p dn -- <command>`.

## Commands

| Command | What it does |
|---|---|
| `dn models list` | List catalogue models and which are downloaded, models you downloaded from Hugging Face, and models already in your Hugging Face cache |
| `dn models pull <id>` | Download a catalogue model. Resumable; checked against its SHA-256 |
| `dn models search <text>` | Search Hugging Face for GGUF models |
| `dn models files <owner/name>` | List the size and quality versions (quantisations) of a Hugging Face model, and its licence |
| `dn models pull <owner/name> --quant Q4_K_M` | Download one version of a Hugging Face model, at a fixed revision |
| `dn models open <link>` | Open a `demios://models/add?…` link: show the Hugging Face model it points at, its licence and whether it fits, then download it only if you type `y` |
| `dn fit <model>` | Say whether a model fits this machine and which settings to use |
| `dn run -m <model> "prompt"` | Chat with a model in the terminal |
| `dn run --endpoint <url> -m <model> "prompt"` | Chat with a model on an OpenAI-compatible server, such as Ollama or llama-server |
| `dn bench <model>` | Measure speed and peak memory, and write a JSON report. `--quality` also runs 14 short prompts with checkable answers |
| `dn bench default --endpoint <url>` | The same against an OpenAI-compatible server, such as mistral.rs or llama-server. `--server-pid` measures the server's memory |
| `dn bench-report <folder>` | Turn `dn bench` reports into a table |
| `dn bench-embed <model>` | Measure an embedding model on English and German questions (recall, speed, memory) |

`<model>` is a catalogue id or a path to a GGUF file.

`--endpoint` follows the same address rules as the app's model servers:
- `http://` works only for this computer and your local network: `localhost`, `127.0.0.1`, `192.168.x.x`, `10.x.x.x`, `172.16–31.x.x`, Tailscale `100.64–127.x.x`, or names such as `nas` and `studio.local`.
- Any other server needs `https://`.
- If the address has no path, `/v1` is added.
- A redirect to another address stops the request.

## Add-model links

A `demios://models/add?…` link names a Hugging Face model, and can name one version of it. The app opens them too (see [Adding a model](models.md#links-that-add-a-model)). `dn models open` never downloads without your `y`, and if the link doesn't pick one version, it lists the `dn models pull` commands for each one instead.

## Settings chosen for you

`dn run` and `dn bench` pick the context size, cache type and GPU layers that fit your machine, using the same estimate as `dn fit`. For a mixture-of-experts model that doesn't fit on the GPU, they keep some or all of its experts in CPU memory (`dn fit` prints this as the `preset` line); `--moe-cpu all` or `--moe-cpu <layers>` sets it yourself. Any option you set yourself wins. A model that won't fit is refused; pass `--force` to try anyway. `--no-auto` turns the automatic choice off.

## The model check

Before a local model is used for the first time, and again whenever its file changes, `dn` checks it for known issues:

- bytes hidden after the model's data;
- settings that hold a large block of data;
- a damaged file layout;
- a file in the Hugging Face cache that no longer matches its hash, or points outside the cache;
- a chat template with web addresses, shell commands, text that instructs the model, encoded blocks or invisible characters.

A model with a problem in the first group is never loaded, and the message says why. A model with only Warnings, such as an unusual chat template, is loaded only with `--accept-model-warnings`. Results are kept in the models folder, so a large file is hashed only once.

"Checked for known issues" means none of the problems above were found. It doesn't mean a model is safe.

## Faster answers (preview)

- **Prompt reuse (on by default):** the model keeps the last request's tokens, and recent other ones in memory. A request that starts like an earlier one, such as the next turn of a chat, only processes what's new. Other requests get the smaller of 2 GB and a tenth of your RAM (about 800 MB on an 8 GB laptop); `--prompt-cache-mib` changes it, and `--no-prompt-cache` turns reuse off. Models with recurrent layers can't do this; `dn run -v` says so. The Demios app uses the same default.
- The draft model below is off unless you ask for it:
- **`--draft <file>`:** a smaller model of the same family suggests the next few tokens and the model checks them all at once, so it writes faster. The answer is the same as without it. The two must share a vocabulary (for example Qwen3 32B with Qwen3 8B); if they don't, the model doesn't load and the message says what differs. The draft model needs memory of its own, which the automatic settings don't count yet.

## Demios Node (early preview)

A Node is the Demios engine running as a service on a machine you own, such as a home PC, a Mac mini or a server, so background work can go on while the app is closed. It keeps your data on that machine, unencrypted while it works on it, so only run one on hardware you control.

For pairing, what syncs, removing devices and the current limits, see [Your Demios Node](node.md).

```sh
dn node init --data-dir /var/lib/demios --name "Home server" --secrets systemd-credential:demios-node --time-zone Europe/Berlin
dn node run --data-dir /var/lib/demios
```

- `--secrets` chooses where the Node keeps keys. The options are:
  - `keychain`: the system keychain or Windows Credential Manager of the Node's account;
  - `systemd-credential:<name>`: an encrypted file whose key comes from a systemd credential, sealed to the TPM by the unit;
  - `container-secret:<name>`: a Docker or Podman secret;
  - `key-file:<path>`: a key file on another disk;
  - `insecure-plaintext`: test setups only, and every device shows a warning.

  The key for an encrypted file is 32 random bytes, for example `openssl rand -hex 32`.
- `--time-zone` is the IANA time zone for jobs at a time of day, such as `Europe/Berlin`; a 07:00 job stays at 07:00 when daylight saving starts or ends. Without it, `dn node init` uses this system's time zone (`TZ`, or where `/etc/localtime` points). It's kept in `node.json` as `"time_zone"`, and an unknown name is refused.
- **Who can connect:** by default the Node accepts devices on its own network only: private, link-local and loopback addresses. It refuses every other address before any handshake, and logs it. It never asks your router to open a port. To accept other addresses (a VPN or overlay, a forwarded port, or a proxy that passes TLS through), add `--reachable-from-outside` to `dn node init`. Anyone who can reach the port can then try to connect, though only paired devices get in. Settings → Node shows "Reachable from outside your network" while this is on. `--listen` only picks the address and port to listen on; it doesn't widen who may connect. **Behind a proxy or port forwarder on the same machine** (a reverse proxy, Docker's published ports, a cloud load balancer), every connection seems to come from a local address, so the default doesn't limit anything there. Treat such a Node as reachable from outside.
- The Node refuses to start if API keys or tokens are set in environment variables (for example `OPENAI_API_KEY`), and refuses a secret store it can't open. It never falls back to another one.
- **Pairing a device:** while `dn node run` is running, run `dn node pair --data-dir /var/lib/demios --address 192.168.1.20:7443` on the Node, as the same user. It shows a link that works once, for 10 minutes; open it on the device. When the device connects, `dn node pair` shows check numbers. Compare them with the device's screen, and type `yes` only if they match. Anything else, or closing `dn node pair`, adds nothing. On Windows this isn't available yet.
- **In the app:** open **Settings → Node**, choose **Pair a Node**, and paste the link. The app shows the same check numbers; confirm on the Node with `yes`.
- **Synced records at rest:** the projects, chats and messages the Node keeps are encrypted in its database, with a key kept in the Node's secret store. Someone who copies the database or a backup of it, without that key, can't read them. What stays readable: record ids, which messages belong to which chat, sizes, when each device changed something, and the device list. With `insecure-plaintext`, the key is in a plain file next to the database, so this protects nothing.
- **Rotating that key:** while `dn node run` is running, run `dn node rotate-records-key --data-dir /var/lib/demios` as the same user. It makes a new key, re-seals the records with it in batches while devices keep syncing, then deletes the old key. If it stops partway, run it again to go on. A backup of the database made before still opens with the old key, if you kept a copy of the secret store from then. On Windows this isn't available yet.
- If the secret store no longer has the records key (for example, the Node was moved without its keychain), the Node refuses to start and says which key is missing. It never makes a new key in its place.
- **Devices:** while the Node runs, `dn node devices --data-dir /var/lib/demios` lists the paired devices with their ids. `dn node remove-device --data-dir /var/lib/demios <id>` removes one: it can't connect or sync again, and the other devices get new keys when they next sync. Its own copy of your data stays on it, so if you still have it, use **Delete all local data** there first. Use this for a device that's lost, or one that forgot the Node. The same removal is in **Settings → Node** on any paired device.
- **Deleting the Node:** stop the Node, then run `dn node delete-all --data-dir /var/lib/demios`. It lists what it deletes and asks you to type the Node's name (or pass `--confirm "<name>"`). It deletes the Node's keys from its secret store, its database (chats, memory, jobs) and `node.json`, then the data folder if nothing else is in it. Files it didn't make are kept and named. Paired devices keep their own copies; in **Settings → Node** on each, choose **Forget this Node**. Uninstalling the Node doesn't delete its data folder; this command does. On Windows, `devices`, `remove-device` and `delete-all` aren't available yet (#261).
- Not there yet:
  - a QR code in the terminal (the link holds the same thing);
  - a passphrase-protected store (`dn node unlock`);
  - swap and crash dumps can hold decrypted data. Full-disk encryption on the Node still helps.

## Examples

```sh
dn models pull qwen3-4b-q4_k_m
dn fit qwen3-4b-q4_k_m
dn run -m qwen3-4b-q4_k_m "Explain DNS briefly"
dn run --endpoint http://localhost:11434/v1 -m llama3.2 "Hi"
```

Run `dn --help`, or `dn <command> --help`, for every option.
