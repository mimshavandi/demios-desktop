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
