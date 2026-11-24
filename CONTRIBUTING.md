# Contributing

1. Read `docs/traceability.md`. Every change belongs to a task ID (`T-<area>-##`) listed in `docs/workers.md`.
2. Branch `t/<task-id>-<slug>`; commit subjects start with the task ID; the PR body links the doc and pastes the done-check output.
3. Before pushing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `gitleaks git --staged` (or the pre-commit hook).
4. New dependency → row in `docs/dependencies.md` and approval in the PR.
5. Architecture change → new or superseding ADR in `docs/adr/`.

Licensed under Apache-2.0; contributions are accepted under the same licence.
