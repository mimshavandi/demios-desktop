# Evaluating Demios: `dn eval`

`dn eval` measures what Demios does with a model, through the same agent loop the app runs
([ADR-0020](../adr/0020-agent-runtime.md) §8). Every case comes with its typed run result:
why it stopped, the model, tokens in and out, cached tokens, wall time, time to the first
token, steps, and what the context assembler left out (nothing yet; the assembler arrives with
R1). Each run writes one JSON report (`dn.eval.v1`) and sends nothing anywhere: the model is a
local GGUF, or an OpenAI-compatible server you name with `--endpoint`.

This is the first stage (R0). Judging answers, embeddings in the LongMemEval retrieval and
more speed cases come later; see "Not yet" below.

## Parity: golden prompts

```bash
dn eval parity --set evals/golden <model> [--only PREFIX] [--max-tokens N] [--out report.json]
dn eval parity --set evals/golden --holdout <private dir> --out <outside the repo> <model>
```

A set is a folder: `prompts/<category>.yaml` (a list of cases per category), `files/` (small
sample files the cases attach), `tools.yaml` (mock tools with canned results) and, optionally,
`set.yaml` with the set's `today` (an ISO date the model is told in every case).
[apps/cli/eval/parity-example](../../apps/cli/eval/parity-example) shows the format. Each case
has an `id`, a `category` (chat, writing, summarise, project, tools, multistep, memory,
research), a `lang` (en or de), the `turns` of this session (the last one the user's) and what
a good answer has (`expect.must`, `expect.must_not`, `expect.tool_calls.required` and
`forbidden`). Optionally: a project (instructions and files), attachments, mock tools, the
write tools the user allows (`approve`), earlier sessions and `max_steps`.

What each case gets for now:

- a fixed stand-in system prompt (the app's prompt moves into the shared runtime in R1), plus
  the project's instructions;
- project files and attachments, quoted as untrusted excerpts, the way the app quotes project
  files;
- its mock tools: read tools answer from `tools.yaml`, and write tools only when the case
  approves them; any other write call is answered "The user declined this action.";
  a read tool named `calculator` with no canned results is answered by the runner, which does
  the arithmetic (`+ - * /`, parentheses, decimals, `%`);
- no earlier sessions: memory arrives in R3, so memory cases are reported `memory: none` and
  measure the baseline without it.

The report keeps every answer, its tool calls and its run result. It checks
`tool_calls.required` and `forbidden` itself; `must` and `must_not` are for the judge, which
comes later.

The golden set is written separately. A **private holdout** has the same shape, and its ids
start with `h-`. `dn eval` refuses a holdout, or its report, inside a git work tree, so neither
can be committed by accident.

## LongMemEval

[LongMemEval](https://github.com/xiaowu0162/LongMemEval) (MIT) asks questions about a long
history of chat sessions. The data isn't in this repository. Fetch a pinned revision, checked
by SHA-256, into a folder outside it:

```bash
scripts/eval/fetch-longmemeval.sh            # oracle and s (about 290 MB)
scripts/eval/fetch-longmemeval.sh --with-m   # also m (2.7 GB)
```

Then:

```bash
dn eval longmemeval ~/.cache/demios-eval/longmemeval/longmemeval_s_cleaned.json <model> \
  --mode retrieval --top-k 8 --hypotheses hyp.jsonl
```

- `--mode history` puts the whole history in front of the question (reading it directly).
- `--mode retrieval` indexes the sessions with Demios's own search (dn-knowledge, in memory,
  discarded after the run) and gives the model only the best passages, quoted as untrusted
  data the same way the app quotes project files. The report adds the session recall: the share
  of the sessions holding the answer that were retrieved.

Comparing the two modes shows what our pipeline loses or gains against reading everything.
`--hypotheses` writes the answers in LongMemEval's format for its own evaluation script.

## Speed

```bash
dn eval speed <model> [--cases cases.json]
```

Time to first token and tokens per second, p50 and p95 per case, from each run's result. The
built-in cases ([apps/cli/eval/speed.default.json](../../apps/cli/eval/speed.default.json))
are: the first request after loading; then short and 2k-token prompts, cold (a different
prompt each time, so nothing can be reused) and warm (the same prompt after a priming request,
so a backend that reuses prefixes can; `cached_input_tokens` shows whether it did). Cases are
data (`dn.speed.v1`), so new ones are added to a file.

## Not yet

These are tracked as issues:

- judging parity answers (pairwise against reference answers, a fixed judge from another
  model family) and LongMemEval answers;
- earlier sessions in memory cases (with memory, R3);
- embeddings in LongMemEval's retrieval mode (keyword search only for now);
- the app's own system prompt in `dn eval parity` (it moves into the shared runtime in R1);
- speed cases for long prefixes, switching chats, restarts with restored slots and draft
  models (with R2).
