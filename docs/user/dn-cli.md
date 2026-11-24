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
| `dn bench <model>` | Measure speed and peak memory, and write a JSON report |

`<model>` is a catalogue id or a path to a GGUF file.

## Add-model links

A `demios://models/add?…` link names a Hugging Face model, and can name one version of it. Only `dn` can open these links for now; the app can't yet. `dn models open` never downloads without your `y`, and if the link doesn't pick one version, it lists the `dn models pull` commands for each one instead.

## Settings chosen for you

`dn run` and `dn bench` pick the context size, cache type and GPU layers that fit your machine, using the same estimate as `dn fit`. Any option you set yourself wins. A model that won't fit is refused; pass `--force` to try anyway. `--no-auto` turns the automatic choice off.

## Examples

```sh
dn models pull qwen3-4b-q4_k_m
dn fit qwen3-4b-q4_k_m
dn run -m qwen3-4b-q4_k_m "Explain DNS briefly"
dn run --endpoint http://localhost:11434/v1 -m llama3.2 "Hi"
```

Run `dn --help`, or `dn <command> --help`, for every option.
