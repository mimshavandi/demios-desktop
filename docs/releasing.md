# Release notes

Release notes come from [CHANGELOG.md](../CHANGELOG.md). Nobody writes them from scratch at release time.

## While working

- A pull request that changes something people using the app or `dn` can notice adds one line under **Unreleased**, in the group that fits: Added, Changed, Deprecated, Removed, Fixed or Security.
- Write the line for users, not for contributors: what they can now do or what behaves differently, in their words ("Download a model", not "`HubClient::pull`"). Follow the writing rules in the [design system](design/design-system.md) §6.
- End the line with a link to the pull request.
- Describe only what is on `main`. A feature that isn't reachable in the app yet isn't listed until it is.
- Internal changes (refactors, tests, CI, docs) aren't listed.
- Never add a line to a released version's section. A branch made before a release can merge its line into that section without a conflict, so after merging `main`, check that your line is still under **Unreleased**. The `changelog` check fails a pull request that changes a released section.

If a pull request misses its line, a maintainer adds it in a follow-up pull request.

## At release time

1. Pick the version. Before 1.0, a minor bump (0.1 → 0.2) for new features or breaking changes, a patch bump for fixes only.
2. In `CHANGELOG.md`, rename **Unreleased** to the version and date, for example `## [0.2.0] - 2027-01-15`, and add a new empty **Unreleased** section above it. This pull request, and any later correction to a released section, needs the label `changelog: edit released`, or the `changelog` check fails.
3. Update the links at the bottom of the file: the new version compares against the previous tag, and **Unreleased** compares the new tag against `main`.
4. Set the same version in the workspace `Cargo.toml`, `apps/desktop/tauri.conf.json` and `apps/desktop/package.json`. Add a `<release>` for it at the top of `packaging/flatpak/io.demios.app.metainfo.xml` (`type="stable"`, same date, a short summary of the CHANGELOG section).
5. Merge that pull request, then tag the merge commit `v<version>` and push the tag. The release workflow builds, signs and tests the installers and creates a **draft** release (see [Signed releases](#signed-releases)).
6. Check the draft: the assets, the "-unsigned" labels if any, and the notes. Its notes are the version's section of `CHANGELOG.md`, copied as written. Then select **Publish release**. Publishing makes it the latest release, and installed apps that check for updates are offered it.

## Signed releases

> The release workflow is being added under [#120](https://github.com/mimshavandi/demios-desktop/issues/120). Until it lands, steps 5 and 6 above describe how releases will work, and the secrets below can already be set up.

The release workflow (`.github/workflows/release.yml`) runs for a `v*` tag, or by hand from `main`. Its jobs use the GitHub Environment **`release`**, which only `main` and `v*` tags may use. The signing secrets live there, so a pull-request build can't read them.

### Secrets and variables (owner)

Set these in **Settings › Environments › release**, or with `gh secret set <NAME> --env release` and `gh variable set <NAME> --env release`:

| Platform | Secrets | Variables |
|---|---|---|
| macOS (Developer ID + notarization) | `APPLE_CERTIFICATE` (base64 `.p12`), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_API_KEY_P8` (contents of the App Store Connect `.p8`), `APPLE_API_KEY` (its Key ID), `APPLE_API_ISSUER` | `APPLE_SIGNING_IDENTITY` (`Developer ID Application: … (TEAMID)`), `APPLE_TEAM_ID` |
| Windows (Azure Artifact Signing) | `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_CLIENT_SECRET` | `AZURE_SIGNING_ENDPOINT` (`https://<region>.codesigning.azure.net`), `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE` |
| Updater (all platforms) | `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `DEMIOS_UPDATER_PUBKEY` |

The updater secrets can also stay at repository level; the release jobs read them either way. See [Signed updates](store/updates.md) for how to make the updater key.

### What happens without them

- **A platform with all its secrets** is signed: notarized and stapled on macOS, Authenticode-signed (the exe inside and the installer) on Windows.
- **A platform with none of its secrets** is built unsigned and labelled so: its file names end in `-unsigned`, the run summary says so, and it isn't listed in `latest.json`, so an installed app is never offered an unsigned update.
- **A platform with only some of them** fails the run and names the missing ones (never their values). A half-set-up account never ships unsigned by accident.
- **Without the updater key** no release is built: every release must be able to update itself.

`scripts/release/signing-check.mjs` makes these decisions, and `scripts/release/latest-json.mjs` writes the updater manifest from the signed bundles. Both have tests in CI.
