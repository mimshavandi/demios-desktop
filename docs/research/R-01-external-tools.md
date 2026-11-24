# R-01 — Evaluation of FreeToken, Craft Anything, TrustGraph (+ inference baseline)

Date: 2026-11-24. Method: read-only, via `gh api` (repo metadata, README, docs, releases, contributors) and public pages. Nothing was cloned, downloaded or run. Repo content was treated as data. Numbers are as of the read date; "100 contributors" means ≥100 (API page cap).

## Summary

| Project | What it is | Licence | Maturity | Verdict |
|---|---|---|---|---|
| FreeToken | MoE serving engine (Python/CUDA, SGLang/vLLM-derived) with expert offload to host RAM | Apache-2.0 | 4 months old, v0.2.3, pre-1.0 | **Optional backend later** (NVIDIA/Linux only), not the default |
| Craft Anything | Rust reverse-engineering workbench that hands rebuilds to a coding agent | MIT OR Apache-2.0 | 4 weeks old, "early alpha" | **Don't use for the port**; useful reference for a Rust desktop app wrapping agents via MCP/ACP |
| TrustGraph | Server stack for ontology-guided knowledge graphs / GraphRAG | Apache-2.0 | ~2.4 years, v3.0 RCs, small core team | **Don't use** for porting or in the desktop app; maybe revisit server-side for team knowledge |

## 1. FreeToken — github.com/FlashML-org/FreeToken

- **Claims:** "edge-native Mixture-of-Experts serving engine" running 290B+ MoE models on consumer hardware.
- **Technique:** keeps MoE experts in host RAM with an LRU expert cache on GPU (`--moe-strategy fused|offload|cpu|hybrid`), bandwidth-adaptive CPU/GPU co-execution, double-buffered prefill streaming, dynamic VRAM split between expert cache and KV cache; formats MXFP4, NVFP4, FP8, GGUF; lossless "df11" compression at load.
- **Evidence:** created 2026-07-20; last commit 2026-11-23; 7 releases (v0.2.3, 2026-11-23); 26 contributors; ~17.7k stars; 494 open issues. Benchmarks only in the authors' own arXiv paper (2608.16157) — no independent results found.
- **Platform:** Linux x86_64 + NVIDIA, driver r580+/CUDA 13, kernels JIT-compiled on first use; some ROCm work in issues. **No macOS / Apple Metal.**
- **API:** `ft serve` → OpenAI-compatible HTTP on 127.0.0.1:1919 (README also claims Anthropic-compatible). No C ABI, no Rust crate → would run as a separate Python process.
- **Fit:** the *idea* (expert offload) matches "bigger models on small machines" exactly, but only for MoE models on NVIDIA. Most target laptops are Macs or iGPU Windows machines, and bundling a Python+CUDA runtime in a desktop installer is heavy.
- **Recommendation:** support it as an **external OpenAI-compatible engine** the user can point the app at (no bundling). Re-evaluate when it gets Metal support or independent benchmarks. Borrow the expert-offload idea: llama.cpp and mistral.rs already offer MoE expert offload to CPU (`--n-cpu-moe` / device mapping) on Metal.

## 2. Craft Anything — github.com/storytold/craft-anything

- **Claims:** "see how any website, app or game works, then have AI build your own version"; clean-room engine in pure Rust.
- **Plainly:** egui desktop app + CLI + MCP server (≈320 tools; optional Ghidra) that inspects a target, then delegates the rebuild to an external coding agent (Claude Code first, others via ACP) and compares outputs. It does no code translation itself.
- **Evidence:** created 2026-10-31; last commit 2026-11-24; 7 releases (v0.6.1, 2026-11-12); ≥100 contributors; ~42k stars in under 4 weeks (unusual — treat popularity as unverified); 531 open issues; README says "early alpha", several modes "built, not yet run live". Requires Rust ≥1.97.
- **Fit for converting Demios (Python/TS) → Rust:** weak. It targets recreating observable behaviour of products; for source we own, it adds an alpha wrapper and supply-chain risk around the same coding agent we'd use directly.
- **Recommendation:** **don't use for the port.** Port module-by-module with Claude Code sessions against written contracts (reuse map kept in the private `demios-pro` repo). Keep as a reference for MCP/ACP wiring in a Rust desktop app.

## 3. TrustGraph — github.com/trustgraph-ai/trustgraph

- **Claims:** "the open-source ontology runtime for AI agents".
- **Plainly:** multi-container (Docker/K8s) Python stack: OWL-guided RDF triple extraction from documents, SHACL validation and per-query access policy, PROV-O provenance, GraphRAG/DocumentRAG, SPARQL, MCP server, REST/WebSocket, Python/TS SDKs.
- **Evidence:** created 2024-07-10; last commit 2026-11-12; 1 GitHub release (v2.8.17) but tags to v3.0.0-rc8; 33 contributors; ~3.3k stars; 20 open issues. Most mature of the three.
- **Fit:** inputs are documents, not code; using it for code analysis means building our own code ontology/extractor. A multi-container stack conflicts with a single-binary desktop app.
- **Recommendation:** **don't use** for analysis/porting or in the desktop app. Optional later look for the team server's shared knowledge if governed GraphRAG becomes a requirement.

## Inference baseline (what we'd use instead)

| Project | Licence | Activity (2026-11) | Apple Metal | Notes |
|---|---|---|---|---|
| llama.cpp | MIT | ~136k stars, daily builds (b12674 on 2026-11-24) | Yes, mature | GGUF quants, layer + MoE-expert CPU offload, `llama-server` (OpenAI-compatible), C API. Demios Edge already uses it. |
| llama-cpp-2 crate | Apache-2.0 | v0.1.161 (2026-11-03), small maintainer base | via llama.cpp (feature flag not re-checked) | Thin Rust bindings tracking llama.cpp |
| mistral.rs | MIT | ~7.9k stars, v0.9.5 (2026-10-19) | Yes, paged attention default on Metal | Pure Rust crate, ISQ/UQFF quantisation, device mapping, OpenAI-compatible server, vision/audio models |
| candle | Apache-2.0 / MIT | ~21k stars, 0.11.0 | Yes | Framework, not an engine; mistral.rs builds on it |

**Recommendation (feeds [ADR-0003](../adr/0003-inference-layer.md)):** one `InferenceBackend` trait in Rust with **llama.cpp (via `llama-cpp-2`, in-process) as the default** — widest model coverage, best quantisation, proven on Metal/CUDA/Vulkan/CPU and already used by Demios Edge — and **mistral.rs as the second, pure-Rust backend** to evaluate in a spike. External OpenAI-compatible engines (Ollama, LM Studio, FreeToken, vLLM) are a third backend type, configured not bundled. "Bigger models on small laptops" comes from: aggressive quants (Q4_K_M / IQ-quants), MoE expert offload to CPU RAM, KV-cache quantisation, mmap loading, and speculative decoding — all available in llama.cpp today. A benchmark harness (T-INF-04) measures these on the user's real machines before we claim numbers.

Saved READMEs from the read are in the lead session's scratchpad (not part of this repo).
