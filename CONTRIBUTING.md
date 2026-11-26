# Contributing

1. Maintainers track work privately as task IDs (`T-<area>-##`, for example `T-SHELL-07`). If you don't have one, open an issue or a PR describing the change, and a maintainer will assign an ID.
2. Branch `t/<task-id>-<slug>`; commit subjects start with the task ID; the PR body says what changed and pastes the output of the checks below.
3. Before pushing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `gitleaks git --staged` (or the pre-commit hook).
4. New dependency → row in `docs/dependencies.md` and approval in the PR.
5. Architecture change → new or superseding ADR in `docs/adr/`.

This repository is licensed under the PolyForm Shield License 1.0.0 ([LICENSE](LICENSE), ADR-0017). By contributing, you license your contribution to DigiNote, Inc. under the Apache License 2.0, and you confirm you have the right to do so (Developer Certificate of Origin: sign off your commits with `git commit -s`). DigiNote, Inc. may then publish it as part of the project under the project's licence.
