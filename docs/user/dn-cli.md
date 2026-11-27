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

## Demios Node (early preview)

A Node is the Demios engine running as a service on a machine you own, such as a home PC, a Mac mini or a server, so background work can go on while the app is closed. It keeps your data on that machine, unencrypted while it works on it, so only run one on hardware you control.

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
- Not there yet:
  - a QR code in the terminal (the link holds the same thing);
  - a passphrase-protected store (`dn node unlock`);
  - encrypting the Node's database file. Until then, use full-disk encryption on the Node.

## Examples

```sh
dn models pull qwen3-4b-q4_k_m
dn fit qwen3-4b-q4_k_m
dn run -m qwen3-4b-q4_k_m "Explain DNS briefly"
dn run --endpoint http://localhost:11434/v1 -m llama3.2 "Hi"
```

Run `dn --help`, or `dn <command> --help`, for every option.
