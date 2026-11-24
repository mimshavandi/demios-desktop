# CLAUDE.md

Guidance for Claude Code sessions in demios-desktop.

- This is the open-source (Apache-2.0) desktop app and shared crates of the Demios split. Plan and decisions: `docs/` (start at `README.md`, `docs/workers.md`, `docs/decisions.md`).
- Work only on the task IDs in your brief; one worker per set of crates. Report to the lead session, not the owner.
- `dn-core` types are contracts: change them only with an ADR reference, and tell the lead.
- No `unsafe` (workspace lint forbids it); only `dn-inference` may opt out for the llama.cpp FFI, with a comment per block.
- Never commit secrets, model files or `.env`. Keys live in the OS keychain (`dn-secrets`).
- Gates before every PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Never copy code or text from the private Demios repo; port contracts and cite `demios@<sha>:<path>` in the PR body.
- Don't add dependencies outside D-012 without asking the lead; record every one in `docs/dependencies.md`.
