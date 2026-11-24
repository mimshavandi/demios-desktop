# Traceability convention

Every piece of work in demios-next can be traced from a decision to a task to a commit and back.

## IDs

| Prefix | Meaning | Lives in | Example |
|---|---|---|---|
| `D-###` | Decision (open or made) | [decisions.md](decisions.md) | `D-004` licence |
| `ADR-####` | Architecture decision record | [adr/](adr/) | `ADR-0002` desktop shell |
| `T-<area>-##` | Task | [workers.md](workers.md) task table | `T-INF-01` |
| `R-##` | Research note | [research/](research/) | `R-01` external tools |
| `W-<name>` | Worker session | [workers.md](workers.md) | `W-inference` |

Areas for task IDs: `GOV` (repo, licence, CI), `CORE` (shared Rust core), `INF` (inference), `SHELL` (desktop UI), `ART` (artifacts), `AGT` (agent runtime / skills), `CON` (connectors / MCP), `TWIN` (executive assistant), `TEAM` (team server), `PRV` (BYOK providers), `KNO` (knowledge index), `DES` (design system), `PORT` (porting from Demios).

## Every task has four fields

1. **ID**: `T-<area>-##`, never reused.
2. **Owner session**: the worker session name (`W-…`) and its session link once started.
3. **Doc link**: the ADR or doc section that says *why* and *what*.
4. **Done-check**: a command or observable result anyone can repeat (e.g. `cargo test -p dn-inference` passes and `dn run --model x.gguf "hi"` streams tokens).

## In git

- Branch: `t/<task-id>-<slug>`, e.g. `t/T-INF-01-engine-trait`.
- Commit subject starts with the task ID: `T-INF-01: add InferenceEngine trait`.
- PR title starts with the task ID; the PR body links the doc and pastes the done-check output.
- A decision that changes during work gets a new `D-###` entry (or a superseding ADR); never silently edit an accepted ADR.

## Status values

`proposed` → `agreed` → `in-progress` → `review` (lead session checking done-check) → `done`. Only the lead session moves a task to `done`, after re-running the done-check.

## Provenance of ported code

Code or contracts taken from the Demios repo cite the source path and commit in the PR body (`from demios@<sha>:<path>`; the Demios repo is private, so the citation is for maintainers with access). Nothing is copied wholesale: the new repos must start with clean history and pass gitleaks from the first commit.
