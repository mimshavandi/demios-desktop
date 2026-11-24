# ADR-0003: Inference layer — backend trait, llama.cpp default

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner

## Context
Users bring their own model: local or BYOK. Goal: run **bigger models on small laptops**. Demios Edge already proved llama.cpp on Apple Silicon, but only on macOS and without streaming or tool calls. FreeToken was evaluated: strong MoE-offload technique, but Linux+NVIDIA+CUDA 13 only, Python runtime, 4 months old.

## Decision
A single `InferenceBackend` trait in `dn-inference`:
```rust
trait InferenceBackend {
    fn capabilities(&self) -> BackendCaps;            // chat, embed, vision, stt, tts; tool calls; grammar
    async fn load(&self, spec: &ModelSpec, opts: &LoadOptions) -> Result<ModelHandle>;
    async fn chat(&self, h: &ModelHandle, req: ChatRequest) -> Result<TokenStream>; // streaming, tools
    async fn embed(&self, h: &ModelHandle, input: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn unload(&self, h: ModelHandle) -> Result<()>;
}
```
Backends, in order:
1. **`llamacpp`** (default) — in-process via `llama-cpp-2` bindings; Metal / CUDA / Vulkan / CPU. Fallback: bundled `llama-server` sidecar if bindings lag.
2. **`mistralrs`** — pure-Rust spike; adopt as second backend if the benchmark (T-INF-04) shows parity on target laptops.
3. **`openai_compat`** — any external server: Ollama, LM Studio, vLLM, **FreeToken** (for users with NVIDIA GPUs and huge MoE models), a team inference box.
4. Remote BYOK providers live in `dn-providers` behind the same `ChatRequest`/`TokenStream` types.

**"Bigger models on small laptops"** is delivered by: GGUF K/IQ quants; mmap loading; **MoE expert offload to CPU RAM** (keep attention + shared layers on GPU — the FreeToken idea, available in llama.cpp); KV-cache quantisation (q8/q4); context sizing from the fit estimator; speculative decoding with a small draft model (Demios Edge already pairs drafts); idle unload. The fit estimator (ported from `ModelFootprintEstimator.swift`) recommends quant + context + offload per machine.

## Options considered

| Option | Metal | Win/Linux | Rust-native | Model coverage | Big-model tricks | Risk |
|---|---|---|---|---|---|---|
| **llama.cpp (llama-cpp-2)** | Yes | Yes | Bindings (C++) | Widest | Quants, MoE offload, spec. decoding | Bindings small maintainer base |
| mistral.rs | Yes | Yes | Yes | Good | ISQ, device mapping, paged attn | Smaller community |
| candle direct | Yes | Yes | Yes | Build your own | Manual | Too low-level |
| FreeToken | **No** | Linux+NVIDIA | No (Python) | MoE focus | Expert offload, FP4 | Young, heavy runtime |
| Ollama as dependency | Yes | Yes | No (Go) | Wide | Inherits llama.cpp | Not embeddable, separate install |

## Trade-offs
llama.cpp is C++, not Rust — accepted: it is the de-facto engine, MIT, and the only one with this coverage today. The trait keeps us free to swap to mistral.rs when it reaches parity.

## Consequences
- One API for local and remote models; agents never know which.
- We own a benchmark harness so claims are measured, not copied from READMEs.
- Revisit FreeToken when it supports Metal or gets independent benchmarks.

## Action items
T-INF-01 trait + types, T-INF-02 llama.cpp backend with streaming + tool calls, T-INF-03 fit estimator port, T-INF-04 benchmark harness (llama.cpp vs mistral.rs on 8/16/32 GB machines), T-INF-05 model catalogue + downloads, T-INF-06 openai_compat backend.
