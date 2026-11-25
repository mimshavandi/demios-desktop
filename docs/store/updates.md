# Signed updates (direct downloads)

Direct downloads from GitHub Releases (macOS DMG, Windows MSI/NSIS, Linux AppImage) can update themselves with the Tauri updater. Microsoft Store (MSIX) and Flathub builds can't: their store updates them, so they are built without the updater. This page is for maintainers: how it works, the one-time key setup, and what a release has to publish.

## How it works

- **Only when asked.** The app contacts GitHub only when someone selects **Settings › General › About › Check for updates**. It doesn't check at launch, and nothing downloads in the background.
- **Only after a confirm.** A newer version shows its number, date and release notes. **Install and restart** asks first. Then the app downloads the update, checks its signature against the public key built into the app, installs it and restarts. If the signature doesn't match, nothing is installed.
- **The signature covers the version.** `requireSignedVersion` is on, so the signature names the version it was made for. Someone who changes `latest.json` can't pass an older, validly signed release off as a newer one. `tauri build` writes the version into the signature on its own.
- **Where it looks:** `https://github.com/mimshavandi/demios-desktop/releases/latest/download/latest.json`. GitHub redirects `releases/latest/download/<file>` to that file on the newest release that isn't a draft or a pre-release. The endpoint is set in `apps/desktop/tauri.conf.json` (`plugins.updater.endpoints`); `updates::ENDPOINT` in the app must match it, and a test checks that.

## Which builds have it

| Build | How it's built | Updates |
|---|---|---|
| Direct download (DMG, MSI/NSIS, AppImage) | `--features updater` and `--config tauri.updater.conf.json`, with the public key | Checks and installs signed updates, when asked |
| Microsoft Store (MSIX) | `DEMIOS_DISTRIBUTION=msix`, without `updater` | The Store updates it |
| Flathub | `DEMIOS_DISTRIBUTION=flatpak`, without `updater` | Flatpak updates it |
| Development and source builds | no `updater` feature | None. About says so |

`apps/desktop/build.rs` fails the build if the `updater` feature is on and `DEMIOS_DISTRIBUTION` is `msix` or `flatpak`, or if `DEMIOS_DISTRIBUTION` has any other unknown value. The MSIX (T-STORE-04) and Flatpak (T-STORE-06) packaging must set `DEMIOS_DISTRIBUTION`.

A direct build with the updater but no public key shows "This build has no update key" and doesn't check.

## One-time setup (owner)

Run this **on your own computer**, never in CI and never in this repository. Use the Tauri CLI version the app pins (`@tauri-apps/cli` 2.11):

```bash
npx @tauri-apps/cli signer generate -w ~/.tauri/demios.key
```

From `apps/desktop` this is the same thing: `npm run tauri signer generate -- -w ~/.tauri/demios.key`. It asks for a password. Use a long one and keep it in your password manager. It writes two files:

- `~/.tauri/demios.key`: the **private key**. Keep it out of every repository, and keep an offline backup, such as an encrypted copy in your password manager. If it's lost, installed apps can't take updates any more: users have to download a new build by hand. If it leaks, someone else could sign updates that installed apps accept.
- `~/.tauri/demios.key.pub`: the **public key**. It isn't secret.

Then:

1. Add two repository secrets in GitHub (Settings › Secrets and variables › Actions):
   - `TAURI_SIGNING_PRIVATE_KEY`: the contents of `~/.tauri/demios.key`.
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: its password.
2. Give the release build the public key, either way:
   - commit the contents of `demios.key.pub` as `plugins.updater.pubkey` in `apps/desktop/tauri.conf.json`, or
   - set it as the build environment variable `DEMIOS_UPDATER_PUBKEY` (for example a repository *variable*, not a secret). It wins over the config.

## What a release must publish (proposed release job)

No release workflow exists yet. When one is added, its direct-download jobs need, per OS:

```bash
DEMIOS_DISTRIBUTION=direct DEMIOS_FEATURES=updater npm run tauri build -- --features updater --config tauri.updater.conf.json
```

`DEMIOS_FEATURES=updater` makes the open-source licences list (`scripts/licenses.mjs`) include the updater's crates. Run it with `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in the job environment, taken from the secrets above. `tauri.updater.conf.json` turns on `bundle.createUpdaterArtifacts`, so the build also writes the update bundle and its `.sig` file. On macOS that's the `.app.tar.gz`, on Windows the `.msi` or `-setup.exe`, on Linux the `.AppImage`.

The job then uploads to the GitHub release:

- the installers and update bundles, with their `.sig` files;
- `latest.json`, in the shape the Tauri updater reads:

```json
{
  "version": "0.2.0",
  "notes": "The version's section of CHANGELOG.md",
  "pub_date": "2027-01-15T10:00:00Z",
  "platforms": {
    "darwin-aarch64": { "signature": "<contents of the .sig>", "url": "https://github.com/mimshavandi/demios-desktop/releases/download/v0.2.0/Demios.Desktop_aarch64.app.tar.gz" },
    "darwin-x86_64": { "signature": "…", "url": "…" },
    "windows-x86_64": { "signature": "…", "url": "…" },
    "linux-x86_64": { "signature": "…", "url": "…" }
  }
}
```

`tauri-apps/tauri-action` can build, sign, upload and write `latest.json` in one step. The MSIX and Flatpak jobs use neither the feature nor the signing secrets.

Never print the key or the password in a log, and never pass them on a command line; use the environment variables.
