# Contributing

1. Maintainers track work privately as task IDs (`T-<area>-##`, for example `T-SHELL-07`). If you don't have one, open an issue or a PR describing the change, and a maintainer will assign an ID.
2. Branch `t/<task-id>-<slug>`; commit subjects start with the task ID; the PR body says what changed and pastes the output of the checks below.
3. Before pushing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `gitleaks git --staged` (or the pre-commit hook).
4. New dependency → row in `docs/dependencies.md` and approval in the PR.
5. Architecture change → new or superseding ADR in `docs/adr/`.

Licensed under Apache-2.0; contributions are accepted under the same licence.
