# ADR-0011: Model sources — Hugging Face Hub first, no in-house registry

**Status:** Accepted (2026-11-24) · **Date:** 2026-11-24 · **Deciders:** project owner · **Extends:** [ADR-0003](0003-inference-layer.md)

## Context
The owner wants open models to come from free public hubs so Demios never runs its own model registry. Demios Edge served a tier-filtered catalogue from its own backend (`GET /models`, SAS/HF download URLs). PR #12 already downloads GGUF files from Hugging Face through a compiled-in, commit-pinned `catalog.json` of 12 models, with resumable SHA-256-verified downloads and a fit estimator.

## Decision
1. **Hugging Face Hub is the primary source.** In the app and `dn`:
   - Search GGUF repos via the public Hub API (`/api/models?filter=gguf&search=…`, sorted by downloads/likes), list a repo's files and quantisations (`/api/models/{repo}/tree/{revision}`), and resolve a **commit SHA** before downloading so a download is reproducible.
   - Verify every file against the SHA-256 from the Hub's LFS metadata (`lfs.oid`); refuse files without it.
   - Run `dn fit` on each candidate quant (from GGUF header bytes via an HTTP range read where possible) and show Ready / Caution / Memory pressure / Won't fit before download.
   - Support split GGUFs (`-00001-of-0000N`), gated repos (user's own HF token stored in `dn-secrets`, never logged), and an `HF_ENDPOINT` setting for mirrors.
   - Show the repo's licence (card metadata) and a link to the model card **before** download; any licence is allowed, with a clear warning when it is missing or non-commercial; record source repo, revision, file and licence in the local model manifest.
   - Reuse models already on disk: read the standard Hugging Face cache (`HF_HUB_CACHE` / `~/.cache/huggingface/hub`) read-only and list matching GGUFs; never delete files we didn't download.
2. **The curated catalogue stays** as a small file in the repo: tested recommendations (model + quant + revision + sha256 + licence + min RAM), not a registry service. Updating it is a normal PR.
3. **Deep links.** Register a `demios://` URL scheme (`demios://models/add?repo=…&file=…&revision=…`) that opens the add-model flow with a confirmation screen (never downloads without the user's click). Registering Demios as a Hugging Face "local app" (upstream `huggingface.js` PR) is **not** done; the scheme is for our own links (website, docs, shared recommendations).
4. **Other sources, without importing their stores:** Ollama, LM Studio, vLLM, FreeToken and similar keep working through the OpenAI-compatible remote backend (ADR-0003). ModelScope is deferred; `HF_ENDPOINT` mirror support covers most regions now.
5. **No telemetry and no Demios server** in any of these flows: requests go from the user's machine to the hub directly.

## Options considered

| Option | Ops burden for us | User choice | Reproducibility | Licence clarity |
|---|---|---|---|---|
| **HF Hub search + pinned revisions + small curated list** | None | Any GGUF on HF | Revision + sha256 | Shown from model card |
| Curated list only (status quo of #12) | Low (PRs) | 12 models | High | High |
| In-house registry service (Demios Edge style) | High (servers, storage, moderation) | What we host | High | We carry the liability |
| Import Ollama/LM Studio stores | Medium (format churn) | Their libraries | Varies | Varies |

## Consequences
- Easier: no backend, no hosting costs, any new GGUF is available on release day, offline reuse of existing HF caches.
- Harder: untrusted repos — mitigated by mandatory sha256 from LFS metadata, HTTPS-only, size/disk checks, GGUF header validation before load (already capped in #12), and licence display. Search ranking and repo naming are outside our control.
- Revisit: other formats (MLX, safetensors) when a backend supports them.

## Action items
T-INF-08 Hub client (search, tree, revision resolve, LFS sha256, split files, gated + token, HF_ENDPOINT) · T-INF-09 remote fit preview from GGUF header range reads · T-INF-10 add-model UI (search, quant picker with fit status, licence before download) in the shell · T-INF-11 HF cache discovery (read-only) · T-INF-12 `demios://` deep link + confirmation screen. Not scheduled: HF local-app registration (decided against), ModelScope (later).
