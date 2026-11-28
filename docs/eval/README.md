# Evaluating Demios: `dn eval`

`dn eval` measures what Demios does with a model, through the same agent loop the app runs
([ADR-0020](../adr/0020-agent-runtime.md) §8). Every case comes with its typed run result:
why it stopped, the model, tokens in and out, cached tokens, wall time, time to the first
token, steps, and what the context assembler left out (nothing yet; the assembler arrives with
R1). Each answering run writes one JSON report (`dn.eval.v1`) and sends nothing anywhere: the
model is a local GGUF, or an OpenAI-compatible server you name with `--endpoint`. Judging is a
separate step (`dn eval judge`, below), and the only one that sends anything: to the judge.

This is the first stage (R0). Pairwise judging, embeddings in the LongMemEval retrieval and
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
  a read tool named `contrast_check` with no canned results is answered with the app's own
  WCAG contrast function (dn-tools), so design cases are scored on real ratios;
- no earlier sessions: memory arrives in R3, so memory cases are reported `memory: none` and
  measure the baseline without it.

Two more case fields:

- `skill: <name>` turns on a chat skill from `skills/library` (or `--skills <dir>`), as the
  message box's skill menu does: its instructions join the system prompt, and its
  `allowed-tools` narrows the case's tools by capability id (`data_profile` is
  `data.profile`; a mock tool can name its own with `capability:`).
- `--local-skills` runs each skill's `SKILL.local.md` (the version for models running on
  this computer, #365) where it has one; its `allowed-tools` must equal `SKILL.md`'s. The
  report records which file each case used.
- `expect.numbers: ["1537.00", "176.3%"]` lists numbers a correct answer states. They're
  checked mechanically at the precision the answer writes them.

`data_profile` and `data_aggregate` are built into the runner, like the calculator: a case
lists them in `tools`, and they read the case's project files and attachments by name and
return dn-data's fact sheet, or one computed figure (a filtered or grouped sum, mean,
median, minimum, maximum or count), exactly as the app does for a project's files. When it ran, the report also checks
that every number in the answer traces to a fact sheet (`data_check`).

The report keeps every answer, its tool calls and its run result. It checks
`tool_calls.required` and `forbidden` itself; `must` and `must_not` are for the judge.

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

The same fixed judge scores the report:

```bash
dn eval judge eval-longmemeval-….json --judge evals/judge/judge.yaml
```

The judge sees each question, the benchmark's expected answer and the response, with a rule
for the question type: abstention questions are right only when the response says it can't
tell; temporal questions allow being off by one in a count of days, weeks or months; for
knowledge updates only the latest information counts; preference questions are right when the
response uses the person's own preferences. `judge.yaml` needs a `longmemeval_prompt`
([longmemeval-v1.md](../../evals/judge/longmemeval-v1.md)). The report gives accuracy overall,
per question type, averaged over types and on abstention questions. A failed answer counts as
wrong, and a judge error makes the report incomplete. It says `method: "dn-judge"`: a score
from LongMemEval's own script is a different method and is never mixed with it. Reports made
before judging existed have no question in them, so answer those again.

## Judging parity answers

```bash
dn eval judge <answers.json> --judge evals/judge/judge.yaml --set evals/golden [--out judged.json]
```

The judge is one fixed model, pinned in a `judge.yaml`: its exact model id, an
OpenAI-compatible endpoint, the variable holding its key (never the key itself) and its prompt,
a versioned file next to it ([evals/judge](../../evals/judge)). There's no default judge;
[judge.example.yaml](../../evals/judge/judge.example.yaml) is refused until a model is filled
in. The judge isn't Claude or GPT, which write the reference answers, nor a family of the
local models under test.

For each case, the judge is shown the conversation, the project's instructions and files, the
tool calls with their results and the answer, all as quoted untrusted blocks, and checks every
`must` and `must_not` (one call, JSON out). A case passes when every `must` is met and no
`must_not` is broken. A case whose answer run failed is `no_answer`; one with no criteria is
`no_criteria`; neither is sent to the judge.

Nothing is guessed: a reply that isn't the expected JSON is retried once, then the case is a
`judge_error` with the raw reply. Any judge error makes the report say `"judged": "incomplete"`
and the command exit with an error (after writing the report). The judged report (`dn.judge.v1`)
records the answers report's SHA-256, the judge's model and its prompt's SHA-256, so reports
from different judges aren't mistaken for each other. The set's `must` / `must_not` must still
match the answers report's.

For a holdout's answers, pass the same `--holdout`, and `--out` outside any git work tree.
When `judge.yaml` names a key variable, the endpoint must be `https` (or plain `http` on this
machine), so the key and the cases never go out unencrypted.

### Pairwise against reference answers

```bash
dn eval judge <answers.json> --judge evals/judge/judge.yaml --set evals/golden \
  --references evals/golden/references
```

Each folder in `--references` is a source (`claude/`, `chatgpt/`), holding `<case id>.md`
and the `meta.json` that `dn eval references` writes (copied into the report). A reference
whose case has changed since it was made is refused, with the cases to make again.
`judge.yaml` then needs a `pairwise_prompt`
([pairwise-v1.md](../../evals/judge/pairwise-v1.md)).

The references come from Claude and ChatGPT through their APIs (ADR-0020 Q5), under the
owner's keys:

```bash
ANTHROPIC_API_KEY=… dn eval references --set evals/golden --source claude \
  --model <exact model id> --out evals/golden/references
OPENAI_API_KEY=… dn eval references --set evals/golden --source chatgpt \
  --model <exact model id> --out evals/golden/references
```

Each case is answered exactly as `dn eval parity` answers it: the same prompt, files and mock
tools. This is the only command besides `judge` that sends anything, and only to that
provider. `meta.json` records the model, the settings and each case's hash as answered. A
case that fails, or answers nothing, writes no file and removes an older one; it's listed in
`meta.json`, and the command ends with an error naming it, to run again with `--only`.
References from two models never share a folder. For a holdout, pass `--holdout` and an `--out`
outside any git work tree.

For every case and source, the judge sees the case and the two answers as A and B, twice: once
with Demios's answer first, once with the reference first. It's a **win** or a **loss** only
when both orders agree; otherwise it's a **tie**, and `orders_disagree` counts how often the
order changed the verdict (position bias). The tool calls aren't shown here, because only
Demios's are recorded.

Per source, the report gives:

- wins, losses and ties;
- `no_answer` (the answer run failed: counted as a loss);
- `no_reference` (left out, never a tie);
- the **win rate** (ties count half);
- the **length-controlled win rate**: a logistic regression of the outcome on the normalised
  length difference, read at equal length, so an answer doesn't win by being longer. With
  fewer than 10 comparisons, or when the fit doesn't converge, there's no figure, and
  `length_controlled_note` says why.

A reference for a holdout's case must also stay outside any git work tree.

### Calibrating the judge

```bash
dn eval calibrate labels.yaml --judge evals/judge/judge.yaml --set evals/golden \
  --out evals/judge/calibration.json
```

`labels.yaml` (`dn.labels.v1`) lists pairs of answers to a case of the set, each with a
person's verdict:

```yaml
format: dn.labels.v1
pairs:
- {id: p001, case: chat-001, a: answers/p001-a.md, b: answers/p001-b.md, better: A}  # A, B or tie
```

The answer files sit next to the labels file. The judge compares each pair as it does with
references (both orders, a preference only when they agree), and the report
(`dn.calibration.v1`) gives the exact agreement, **Cohen's κ** over the three verdicts, the
confusion matrix (person × judge) and how often the order changed the judge's verdict. About
100 labelled pairs is the target (ADR-0020 §8); the κ a judge needs is the owner's call.

Saved as `calibration.json` next to `judge.yaml`, it's quoted in every `dn eval judge` report
that uses references, but only if it was made with the same judge model and pairwise prompt
and is complete. Otherwise the report's `calibration` says why there's none, and the command
says so on stderr. Calibrate on the public set: a calibration with holdout cases (`--holdout`)
quotes them, so it and its labels stay outside any git work tree, and `dn eval judge` never
quotes it.

## Context: how much of its window a model uses well

```bash
dn eval context <model> [--lengths 4096,8192,...] [--samples 5] [--tasks multihop,count] [--reasoning] [--answer-tokens N] [--out report.json]
```

Measures how far into its context a local model still finds what it's asked for, so the
assembler's effective-context default (60% of the loaded context until measured, #350) can be
set per model. The documents are synthetic and generated in the runner, the same every run,
so there's nothing to download. Lengths are counted with the model's own tokenizer, from 4k
up to its training context. At each length:

- **needle:** one fact (an access code) at five depths, two documents each;
- **multikey:** four similar facts, asked for one (`--samples` documents);
- **multihop:** a chain of three notes (`KESTREL = 48213`, `JUNIPER = KESTREL`,
  `OBSIDIAN = JUNIPER`) spread through the document, with two decoy chains, asked for the last
  one's value: two hops (`--samples` documents);
- **count:** 2–5 delivery lines for one dock spread through the document, among lines for
  two other docks, asked how many arrived there (`--samples` documents).

Four hops and counts up to 9 were too hard for 8B models even at 4k, so they measured the task
rather than the context.

An answer is right when it states the expected number. A length is **effective** when its
accuracy, and every shorter length's, is at least 0.85. Finding one fact is easier than
using a whole document, so the effective length rests on the harder tasks, multihop and count,
pooled. The report gives, per length, the accuracy and the number of cases behind it for the
hard tasks, each task and all tasks, the effective length of each, and the effective length's
share of the training context.

**The 2k gate.** Multihop and count first run at 2,048 tokens, on at least 10 documents each. A
task under 0.85 there is reported as too hard for this model (`too_hard`): it isn't run at
longer lengths, gets no effective length, and isn't pooled.

**track** (only with `--tasks track`) checks this runner against a known benchmark's setup:
RULER-style variable tracking, with one chain of four hops (`VAR A = 12345`, `VAR B = VAR A`, …)
in one short passage repeated, asked forward ("which variables hold 12345?"). The start of the
answer is given at the end of the question, and the score is the share of the five names found.
It's much easier than multihop (no decoy chains, no real prose, a forward question), so a
model that does well on track and poorly on multihop finds the harder task hard, not the runner.

`--tasks` runs only some tasks. `--reasoning` lets the model think before answering (only the
answer is scored), with 1,024 answer tokens unless `--answer-tokens` says otherwise.
`empty_answers` counts answers that ran out of tokens before saying anything. It loads the model with a
context just above the longest length, so long lengths need memory and time.

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

Two kinds time what prompt reuse is for:

- `next_turn`: a chat's second turn after a 2k, 8k or 16k-token first message;
- `switch_back`: going back to a chat after another one.

Whether those are cold or warm is the model's setting, not the case's. Prompt reuse is on by
default, so run the suite twice to compare:

```bash
dn eval speed <model> --no-prompt-cache
dn eval speed <model>
dn eval speed <model> --draft <smaller model of the same family>
```

With `--draft`, each run's result also counts the drafted and accepted tokens.

## Not yet

These are tracked as issues:

- the labelled pairs for calibration (#355);
- earlier sessions in memory cases (with memory, R3);
- embeddings in LongMemEval's retrieval mode (keyword search only for now);
- the app's own system prompt in `dn eval parity` (it moves into the shared runtime in R1);
- a speed case for a restart with restored slots (with the Node's saved slots, R2).
