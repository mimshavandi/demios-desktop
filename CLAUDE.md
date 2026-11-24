# CLAUDE.md

Guidance for Claude Code sessions in demios-desktop.

- This is the open-source (Apache-2.0) Demios Desktop app and its shared crates. Public docs index: `docs/README.md`; architecture decisions: `docs/adr/`. Maintainers track tasks and planning privately; your brief names your task IDs.
- Work only on the task IDs in your brief; one worker per set of crates. Report to the lead session, not the owner.
- `dn-core` types are contracts: change them only with an ADR reference, and tell the lead.
- No `unsafe` (workspace lint forbids it); only `dn-inference` may opt out for the llama.cpp FFI, with a comment per block.
- Never commit secrets, model files or `.env`. Keys live in the OS keychain (`dn-secrets`).
- Gates before every PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Never copy code or text from the private Demios repo; port contracts and cite `demios@<sha>:<path>` in the PR body.
- Don't add dependencies outside the approved set in `docs/dependencies.md` without asking the lead; record every one in `docs/dependencies.md`.
- Persistence: crates that store data use `rusqlite` against a `&rusqlite::Connection` the app passes in, and export `pub fn migrations() -> &'static [&'static str]` (their own tables, prefixed by crate, e.g. `artifact_`, `tool_ledger_`). `dn-store` composes and runs them. Don't reach into another crate's tables.
- Chat: local backends and providers implement `dn_core::chat::ChatModel`; the agent loop only uses that trait.
- Shared files (`Cargo.toml` workspace deps, `docs/dependencies.md`) will conflict between workers: rebase on `main` before opening a PR and keep edits to them minimal.
