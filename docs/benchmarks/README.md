# Benchmarks

How fast local models run in Almend on 8, 16 and 32 GB machines, how much memory they use, and
whether they still answer correctly. The numbers decide whether
[mistral.rs](https://github.com/EricLBuehler/mistral.rs) becomes a second in-process backend
next to llama.cpp ([ADR-0003](../adr/0003-inference-layer.md) planned it as "adopt if the
benchmark shows parity on target laptops").

Results go in [`results/`](results/), one folder per machine run. Embedding models for the
knowledge index are compared in [embeddings.md](embeddings.md) (`dn bench-embed`). The decision is recorded as an
ADR once results from all three memory tiers exist.

## What is measured

`dn bench` writes one JSON report per model and backend (schema `dn.bench.v2`), and
`dn bench-report` turns a folder of reports into a markdown table.

| Column | What it is |
|---|---|
| pp t/s, tg t/s | Prompt processing and generation tokens per second with exact token counts (512 and 128), llama-bench style. In-process llama.cpp only. |
| first token ms | Time from sending a request (about 550 prompt tokens) to the first token of the reply. |
| chat t/s | Generation speed after the first token, on the same request. |
| peak RSS GiB | Highest memory use of `dn` (in-process), or of the server process (`--server-pid`). On Apple Silicon, GPU memory backed by the model file counts too. |
| quality | Passed prompts out of the 14 in the [quality set](../../apps/cli/src/quality.json): arithmetic, short facts, following an instruction and JSON output, with greedy decoding and thinking off. |
| placement | Where the fit estimator put the model: all on the GPU, experts in RAM (mixture-of-experts models) or some layers on the GPU. |

The first-token and chat columns send the same request through the same chat code for every
backend, so they are the fair comparison between llama.cpp and mistral.rs. Each run starts its
prompt differently, so a server can't reuse a cached prompt between runs.

The quality set is a smoke test, not a ranking. It catches a backend that breaks a model's chat
template or a quant that damages a model. With greedy decoding, two backends running the same
file should score about the same.

## Run it on your machine

It takes 1 to 3 hours, mostly downloads, and the disk space the script asks for: about 11 GB
for the 8 GB tier, 32 GB for the 16 GB tier and 95 GB for the 32 GB tier. Close other apps first, and keep a laptop
on power.

1. Build `dn` from this repository, in release mode:

   ```sh
   cargo build --release -p dn
   ```

2. Optional, for the comparison: install mistral.rs and llama.cpp's server.
   - mistral.rs: the installer from its README (`curl -fsSL https://mistralrs.dev/install.sh | sh` on macOS and Linux), or `cargo install --path mistralrs-cli --locked --features metal` in a checkout of its repository on a Mac (leave out `--features metal` elsewhere). Check that `mistralrs serve --help` works.
   - llama-server: `brew install llama.cpp` on macOS, or a release from the llama.cpp repository. It is llama.cpp behind HTTP, so comparing it with in-process llama.cpp shows how much the server itself costs.

   Then tell the script how to start them:

   ```sh
   export SERVERS='llama-server|llama-server -m {gguf} --host 127.0.0.1 --port {port} -c 4096 --jinja
   mistralrs|mistralrs serve -f {gguf} --host 127.0.0.1 -p {port}'
   ```

3. Run the suite:

   ```sh
   DN=target/release/dn scripts/bench/run-suite.sh
   ```

   It picks the memory tier from this machine's RAM (`--tier 8|16|32` overrides it), lists the
   models and how much it will download, and asks before it starts. Downloads resume and are
   checked against their SHA-256. A model the fit estimator says won't fit is skipped, and its
   log says so.

   | Tier | Models |
   |---|---|
   | 8 GB | Qwen2.5 0.5B, Llama 3.2 3B, Qwen3 4B, Phi-4 mini, Gemma 4 E2B |
   | 16 GB | the 8 GB set, Llama 3.1 8B, Qwen3 8B, and Qwen3-30B-A3B at Q2_K (a mixture-of-experts model that fits with its experts in RAM) |
   | 32 GB | the 16 GB set, gpt-oss 20B, Mistral Small 24B, Gemma 4 26B-A4B, Qwen3-30B-A3B at Q4_K_M |

4. The script writes its result folder under `bench-results/`. Copy the folder's `*.json` reports
   and `report.md` into a new folder of the same name in `docs/benchmarks/results/`, and open a
   pull request with it, or send it to the maintainers. The reports name the operating system,
   CPU and GPU model, RAM and model files. They don't contain paths, user names or host names.
   Keep the `*.log` files to yourself unless someone asks for them: server logs can contain full
   paths, which include your user name.

To run a single model by hand:

```sh
dn bench qwen3-8b-q4_k_m --quality
dn bench default --endpoint http://127.0.0.1:1234/v1 --server-pid <pid> \
  --backend-name mistralrs --gguf <path to the same .gguf> --quality
dn bench-report . -o report.md
```

## How the mistral.rs decision will be made

mistral.rs becomes a second in-process backend only if, on the same GGUF files:

- its generation speed (chat t/s) is at least that of in-process llama.cpp on every tier, and
  clearly better (about 15% or more) on at least one;
- its first-token time and peak memory are no worse than about 10% above llama.cpp's;
- its quality score matches llama.cpp's within one prompt;
- it runs the models of every tier, including the mixture-of-experts ones, on macOS, and on
  Windows and Linux at least on the CPU.

Otherwise llama.cpp stays the only in-process backend, and mistral.rs stays usable as an
OpenAI-compatible server, like Ollama or LM Studio.
