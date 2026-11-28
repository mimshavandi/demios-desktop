# Skills

Example skills and an example agent in the manifest format from ADR-0005. Schemas:
`crates/dn-agent/schemas/`. Loader: `dn_agent::manifest`.

| Skill | Kind | What it shows |
|---|---|---|
| `text.summarize` | `prompt` | A prompt-only skill: input fields become template variables. No code. |
| `notes.find_and_read` | `tool_chain` | Chains two MCP filesystem tools; `{input.*}` and `{steps.N.output}` substitution. |
| `clock.now` | `builtin` | A Rust handler registered by dotted id. |

`agents/assistant.default/` is an agent that uses all three.

Layout:

```
skills/<skill-id>/skill.yaml
skills/<skill-id>/prompt/manifest.yaml   # prompt skills: versioned prompt
skills/<skill-id>/prompt/v1.md
```

Prompt templates use `{name}` placeholders; `{{` / `}}` are literal braces.
Run `cargo test -p dn-agent` to validate every manifest here.

## Shared prompts (`shared/`)

These are prompt blocks the runtime adds on its own, not skills the user picks. Each one is a versioned prompt with a `CHANGELOG.md`. A new version is tied to the `dn eval parity` run it was checked with.

| Prompt | Used for |
|---|---|
| `answer.rules` | Every turn on a surface that can have sources (chats, research). It covers leading with the answer, citing with block ids such as `[S1]`, and handling missing or conflicting sources. Kept under about 150 tokens, because every chat turn pays for it. |

## Chat skills (`library/`)

A chat skill changes how the assistant works in one chat. The user picks it in the composer's **Skill** menu, and it applies from the next answer on. Each one is a folder with a `SKILL.md`:

```
skills/library/<name>/SKILL.md
---
name: <name>              # lowercase letters, digits and hyphens; same as the folder
description: <one line>   # shown in the picker
allowed-tools: []         # tool patterns, e.g. [artifact.*] or [mcp.files.read_file]
license: Apache-2.0       # optional, SPDX id
---
Instructions in Markdown, added to the system prompt.
```

The front matter follows the Agent Skills `SKILL.md` layout, with one difference: here `allowed-tools` only narrows. A skill can take tools away, but it can't add a tool the assistant doesn't already have, turn on a tool the user switched off, or skip an approval. Approvals, risk tiers and per-tool choices always apply. Unknown front-matter keys are refused. Loader: `dn_agent::chat_skill`. Narrowing: `dn_tools::Narrowed`.

| Skill | Tools | What it does |
|---|---|---|
| `write-email` | none | Drafts a new email or a reply |
| `summarize` | none | Key points and actions from pasted text |
| `compare` | none | Side-by-side table of options, then a conclusion |
| `outline` | artifacts | Outline for a talk, presentation or document |
| `critique` | none | Specific feedback with fixes on writing or a design |
| `design-review` | artifacts, `contrast.check` | Reviews a UI design in seven categories (hierarchy, spacing, type scale, WCAG AA contrast, grid, touch targets, content), with exact fixes. Contrast ratios come only from the `contrast_check` tool, never from the model; without the tool it lists the colour pairs to check |

**Your own skills.** The same files can go in the `skills` folder of the app's data directory: `<data dir>/skills/<name>/SKILL.md`. Settings › General › Skills opens that folder and reloads it. Your skills follow exactly the same rules: `allowed-tools` only narrows, and approvals always apply. A file that doesn't load is listed there with its error (for example, `name: … must match its folder name`). A skill can't take the name of a built-in one. At most 100 load, and each file is limited to 64 KB. "Delete all local data" leaves this folder alone, because the files are yours.

**Shorter versions for local models.** A skill can have a `SKILL.local.md` next to its `SKILL.md`. It has the same front matter, and the same rules said more briefly, for models that run on this computer. Keep the two in step. The app doesn't use `SKILL.local.md` yet; until it does, every model gets `SKILL.md`.

The skill texts are written for this project (Apache-2.0); no wording is copied from elsewhere. `design-review`'s WCAG thresholds are adapted from an MIT-licensed project; see its `NOTICE.md`. `cargo test -p demios-desktop skills::` checks that every file here loads.
