# Release notes

Release notes come from [CHANGELOG.md](../CHANGELOG.md). Nobody writes them from scratch at release time.

## While working

- A pull request that changes something people using the app or `dn` can notice adds one line under **Unreleased**, in the group that fits: Added, Changed, Deprecated, Removed, Fixed or Security.
- Write the line for users, not for contributors: what they can now do or what behaves differently, in their words ("Download a model", not "`HubClient::pull`"). Follow the writing rules in the [design system](design/design-system.md) §6.
- End the line with a link to the pull request.
- Describe only what is on `main`. A feature that isn't reachable in the app yet isn't listed until it is.
- Internal changes (refactors, tests, CI, docs) aren't listed.

If a pull request misses its line, a maintainer adds it in a follow-up pull request.

## At release time

1. Pick the version. Before 1.0, a minor bump (0.1 → 0.2) for new features or breaking changes, a patch bump for fixes only.
2. In `CHANGELOG.md`, rename **Unreleased** to the version and date, for example `## [0.2.0] - 2027-01-15`, and add a new empty **Unreleased** section above it.
3. Update the links at the bottom of the file: the new version compares against the previous tag, and **Unreleased** compares the new tag against `main`.
4. Set the same version in the workspace `Cargo.toml`, `apps/desktop/tauri.conf.json` and `apps/desktop/package.json`. Add a `<release>` for it at the top of `packaging/flatpak/io.demios.app.metainfo.xml` (`type="stable"`, same date, a short summary of the CHANGELOG section).
5. Merge that pull request, then tag the merge commit `v<version>`.
6. Create the GitHub release from the tag. Its notes are the version's section of `CHANGELOG.md`, copied as written. Add download and install notes when there are installers.
