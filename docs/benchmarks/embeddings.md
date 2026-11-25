# Embedding models for the knowledge index (#128)

The local knowledge index embedded project files and questions with nomic-embed-text v1.5, which
is trained mostly on English. This compares it with multilingual GGUF embedding models that run
through dn-inference (llama.cpp), on English and German.

**Decision (#201):** the default search model is now nomic-embed-text v2 MoE at Q5_K_M
(`nomic-embed-text-v2-moe-q5_k_m`, 371 MB). It gets the same results as Q8_0 here at about 70% of
the size (see [Quantisations](#quantisations-of-nomic-v2-moe)).

**Recommendation of the first comparison: nomic-embed-text v2 MoE (Q8_0, `nomic-embed-text-v2-moe-q8_0`).**
- It is already in the catalogue and is Apache-2.0.
- It is the fastest of the multilingual models and second best in quality (0.97 recall@1).
- It needs the same prefixes (`search_query: ` / `search_document: `) and gives the same
  dimension (768) as v1.5, so the index needs no other change.
- The index stores the model id with every vector, so switching re-embeds once.

## How to run it

```sh
dn bench-embed <model GGUF or catalogue id>   # writes embed-<model>.json
```

The set is built into `dn` (`apps/cli/src/embed_set.json`, version 2):
- **Documents:** 36 short project-style documents: 12 English, 12 German, and 12 near-misses.
  Each near-miss shares a topic and often words with a right answer, but answers something
  else: an Android release checklist next to the macOS one, a Porto trip next to the Lisbon
  one, data deletion next to data access requests.
- **Questions:** 36 questions, each with one right document:
  - 12 English → English;
  - 12 German → German;
  - 6 German → English documents (de-en);
  - 6 English → German documents (en-de).
- **Settings:** each model gets the prefixes its model card asks for (table below). Documents
  are embedded one at a time, as the index does.

## Results

On an Apple Silicon Mac (M3 Ultra, Metal), 25 November 2026. The JSON reports are in
[`results/macos-arm64-256gb-20261125-embeddings/`](results/macos-arm64-256gb-20261125-embeddings/).
Speed is for comparison between models; laptops are slower.

| model (GGUF) | size | dim | recall@1 | de | de→en | en→de | MRR | docs/s | peak RSS | licence |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| nomic-embed-text v1.5 Q4_K_M (today) | 84 MB | 768 | **0.64** | 0.75 | **0.17** | **0.17** | 0.68 | 204 | 0.19 GiB | Apache-2.0 |
| **nomic-embed-text v2 MoE Q8_0** | 512 MB | 768 | **0.97** | 1.00 | 1.00 | 0.83 | 0.99 | **149** | 0.75 GiB | Apache-2.0 |
| nomic-embed-text v2 MoE Q4_K_M | 344 MB | 768 | 0.94 | 1.00 | 1.00 | 0.67 | 0.97 | 118 | 0.60 GiB | Apache-2.0 |
| EmbeddingGemma 300M Q8_0 | 333 MB | 768 | **1.00** | 1.00 | 1.00 | 1.00 | 1.00 | 117 | 0.53 GiB | Gemma Terms of Use (custom) |
| bge-m3 Q8_0 | 634 MB | 1024 | 0.97 | 1.00 | 1.00 | 0.83 | 0.99 | 71 | 0.87 GiB | MIT |
| granite-embedding 278M multilingual Q8_0 | 303 MB | 768 | 0.89 | 0.83 | 1.00 | 0.67 | 0.94 | 141 | 0.55 GiB | Apache-2.0 |
| Qwen3-Embedding 0.6B Q8_0 | 639 MB | 1024 | 0.89 | 0.92 | 0.83 | 0.67 | 0.94 | 64 | 1.00 GiB | Apache-2.0 |

Every model gets English → English right; the differences are all in German and across
languages.

- **nomic v1.5:** across languages, the right document lands at rank 10 to 30 of 36, close to
  random. In German it misses a quarter of the questions.
- **All multilingual models:** they find the right document in the top 3 for every question.
- **Misses:** "How often do we test restoring a backup?" (the near-miss about weekly laptop
  backups ranks first) is the one question most of them miss at rank 1.
- **Not loaded:** multilingual-e5-small, which exists only as community GGUF conversions. The
  one tried (`cstr/multilingual-e5-small-GGUF`) doesn't load in this llama.cpp ("null result"),
  so it isn't in the table.

## Choosing

- **nomic v2 MoE Q8_0 (recommended):**
  - near the top in quality, and the fastest multilingual model here;
  - Apache-2.0, already in the catalogue, same prefixes and dimension as v1.5;
  - 512 MB is the cost: 0.75 GiB in memory while indexing.
  - The Q4_K_M file saves 170 MB but loses a little en→de quality, and wasn't faster here.
- **EmbeddingGemma 300M:**
  - the best score, at two thirds of the size;
  - but its licence is Google's Gemma Terms of Use, with a prohibited-use policy. The catalogue
    allows it with a licence warning.
  - Worth offering if the owner is fine with that licence; the recommendation stays with an
    Apache-2.0 model.
- **bge-m3:** as good as nomic v2, but slower and bigger, with 1024 dimensions.
- **granite 278M and Qwen3-Embedding 0.6B:** behind on German and across languages.

## Quantisations of nomic v2 MoE

Every published quantisation, on the same set and machine (`dn bench-embed`). Q5_K_M answers the
same questions right as Q8_0; at Q4 one cross-language question drops from rank 1 to rank 2.

| quant | size | recall@1 | across languages (de→en, en→de) | docs/s |
|---|---:|---:|---:|---:|
| Q8_0 | 512 MB | 0.97 | 0.92 | 148 |
| Q6_K | 397 MB | 0.97 | 0.92 | 147 |
| **Q5_K_M (default)** | 371 MB | 0.97 | 0.92 | 128 |
| Q4_K_M | 344 MB | 0.94 | 0.83 | 144 |
| Q4_K_S | 325 MB | 0.92 | 0.83 | 144 |
| Q3_K_M | 308 MB | 0.92 | 0.83 | 130 |
| Q2_K | 273 MB | 0.67 | 0.25 | 133 |

Speed differences are within run-to-run noise. The app loads v2 at its 512-token training
context; a longer passage is cut there.

## Prefixes each model needs (`dn bench-embed`, `embed_eval::prefixes`)

| model | question | document |
|---|---|---|
| nomic-embed-text v1.5 / v2 MoE | `search_query: ` | `search_document: ` |
| EmbeddingGemma | `task: search result \| query: ` | `title: none \| text: ` |
| Qwen3-Embedding | `Instruct: Given a question, retrieve the passage that answers it\nQuery:` | (none) |
| multilingual-e5 | `query: ` | `passage: ` |
| bge-m3, granite-embedding | (none) | (none) |
