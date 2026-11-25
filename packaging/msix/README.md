# Microsoft Store package (MSIX)

How Demios Desktop is packaged for the Microsoft Store. There is no Partner Center account yet, so the identity values in [`identity.json`](identity.json) are **placeholders**. A package built with them installs for testing, but you can't upload it.

| File | What it is |
|---|---|
| [`AppxManifest.xml.in`](AppxManifest.xml.in) | Manifest template. `{{…}}` values are filled at build time |
| [`identity.json`](identity.json) | Placeholder identity values. Real values come from environment variables and are never committed |
| [`build-msix.mjs`](build-msix.mjs) | Builds the release app, lays out the package and packs it |
| [`build-msix.test.mjs`](build-msix.test.mjs) | Tests for version, identity, capabilities and the manifest. They run on any OS |
| [`../../.github/workflows/msix.yml`](../../.github/workflows/msix.yml) | Manual CI job (it also runs on PRs that change this folder). It builds an unsigned test `.msix` and runs the App Certification Kit |

## How the package is made

Tauri 2 builds MSI and NSIS installers, not MSIX. So the script uses the Windows SDK tools directly:

1. `tauri build --no-bundle` builds the release app (embedded UI, licences list) with no installer.
2. The script makes a package folder (`target/msix/layout/`) holding:
   - `demios-desktop.exe`;
   - the `bundle.resources` from `tauri.conf.json` (the `skills/` folder), next to the exe, where Tauri looks for resources on Windows;
   - tile and logo images, drawn from `apps/desktop/icons/icon.png` at every size the Store asks for;
   - `AppxManifest.xml`.
3. `makepri` indexes the image sizes, and `makeappx pack` makes `DemiosDesktop_<version>_x64.msix`.

The package is **unsigned**. The Store signs the packages it publishes, so the Store path needs no code-signing certificate of our own.

We didn't use the MSIX Packaging Tool. It is a GUI that records an installer running on a clean machine. It can't be scripted reproducibly in CI, and it would package the MSI's install steps rather than the app.

## Manifest choices

- **Win32 desktop app with full trust.** `EntryPoint="Windows.FullTrustApplication"` and the `runFullTrust` capability are what every desktop bridge app (a normal `.exe`) needs. Without them the package can't start the exe.
  - The app runs as a normal desktop process, not in an AppContainer. That's what it needs to:
    - run local AI inference;
    - start the local tool servers (MCP) the user configures, such as `npx` or `uvx` processes;
    - use the OAuth loopback listener on `127.0.0.1`;
    - store keys in Credential Manager.
  - `runFullTrust` is a restricted capability. Give the reason in the submission notes (wording in [`docs/store/review-notes.md`](../../docs/store/review-notes.md)).
- **`internetClient`.** The app connects to:
  - the providers the user adds keys for (Anthropic, OpenAI, Gemini);
  - Hugging Face, for model lists and downloads;
  - HTTP MCP servers and their OAuth servers.

  A full-trust app isn't network-isolated, so this capability mostly tells the Store listing that the app uses the internet.
- **Not declared:** `privateNetworkClientServer`, `internetClientServer`, file-system, device or other restricted capabilities.
  - LAN MCP servers and local inference servers work without them under full trust.
  - Nothing in the app needs file, device or other system access.
  - The capability set is checked by `build-msix.test.mjs`. Add a capability only with a reason in the manifest comment and here.
- **`demios://` links** are registered through a `windows.protocol` extension, the MSIX equivalent of what the MSI/NSIS installers write. A release build never registers the scheme itself (`deeplink.rs`).
- **Windows 10 1809 or later** (`MinVersion 10.0.17763.0`), the oldest version the WebView2 runtime supports. x64 only for now.
- **WebView2 runtime.** It ships with Windows 11 and current Windows 10. An MSIX can't run the WebView2 bootstrapper. On a machine without the runtime the app won't start: test this on a clean VM before the first submission.

## Store builds don't update themselves

The Store updates the app itself. A self-updater in the package is not allowed.

- The updater task (#58) adds the updater behind a build flag that only direct-download builds turn on.
- `build-msix.mjs` builds with no extra features and refuses the self-update feature (`STORE_FORBIDDEN_FEATURES`, currently `updater`). Keep the name in step with #58.
- If the updater also needs a `plugins.updater` block in `tauri.conf.json`, the Store build must leave it out or turn it off. Check this when #58 lands.

Artifacts stay off on Windows in this package, as in every Windows build. Threat model R8 (#75) explains why.

## Data under MSIX

MSIX redirects the app's writes to `%APPDATA%` into the package's own folder:

- the database and settings;
- caches written by MCP servers the app starts.

That folder is `%LOCALAPPDATA%\Packages\<package family name>\LocalCache\Roaming\io.demios.app\`.

- Settings › About may show the unredirected path.
- The `dn` command-line tool, run outside the package, won't see the same database.
- Uninstalling removes the folder.
- Keys in Credential Manager are not removed.

Check this on the clean-VM test.

## Build locally (Windows)

You need:
- the Windows 10/11 SDK, for `makeappx`, `makepri` and `signtool`;
- Node 22;
- Rust;
- `cargo-about`, which the release licences list needs: `cargo install cargo-about --version 0.9.3 --locked --features cli`.

From the repo root:

```powershell
npm ci --prefix apps/desktop
npm ci --ignore-scripts --prefix ui/artifact-sandbox
node packaging/msix/build-msix.mjs
```

The package is written to `target\msix\`.
- `--skip-build` packs an existing `target\release` build.
- `--out <dir>` changes the output folder.
- `WINDOWS_SDK_BIN` points at a specific SDK `bin\<version>\x64` folder.
- On macOS or Linux the script prints the rendered manifest and stops, because packing needs Windows.

Tests (any OS):

```bash
node --test packaging/msix/build-msix.test.mjs
```

## Sideload test with a dev certificate

Windows installs only signed packages. For a local test, sign a copy with a self-signed certificate. Make it on your machine and **never commit or share it**. `.pfx` and `.cer` files are git-ignored here.

In an elevated PowerShell:

```powershell
# The certificate's subject must equal the manifest's Publisher.
$publisher = ([xml](Get-Content target\msix\layout\AppxManifest.xml)).Package.Identity.Publisher
$cert = New-SelfSignedCertificate -Type Custom -Subject $publisher -KeyUsage DigitalSignature `
  -FriendlyName 'Demios MSIX dev' -CertStoreLocation Cert:\CurrentUser\My `
  -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
Export-Certificate -Cert $cert -FilePath $env:TEMP\demios-msix-dev.cer
Import-Certificate -FilePath $env:TEMP\demios-msix-dev.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople

Copy-Item target\msix\DemiosDesktop_*_x64.msix $env:TEMP\demios-dev.msix
& "${env:ProgramFiles(x86)}\Windows Kits\10\bin\<version>\x64\signtool.exe" sign /fd SHA256 /sha1 $cert.Thumbprint /s My $env:TEMP\demios-dev.msix
Add-AppxPackage $env:TEMP\demios-dev.msix
```

Then check, ideally on a clean Windows VM:
- the app starts from the Start menu;
- a BYOK chat works;
- a stdio MCP server (`npx …`) starts and stops with the app;
- a `demios://models/add?…` link opens the confirmation screen;
- Delete all local data works;
- uninstalling removes the data folder.

Clean up afterwards:
1. Run `Remove-AppxPackage` on the package.
2. Delete the certificate from `Cert:\CurrentUser\My` and `Cert:\LocalMachine\TrustedPeople`.

## CI test package

Actions › **msix** › *Run workflow* builds the unsigned package with the placeholder identity. If the runner has the Windows App Certification Kit (WACK), the job also:
1. signs a copy with a throwaway certificate made in the job, whose key never leaves the runner;
2. runs the kit on that copy.

The artifact `msix-test-package` holds the unsigned `.msix`, the rendered manifest and the WACK report. It's kept for 14 days. The job is a test, not a release step.

## What the owner has to do

1. **Partner Center account.** Open a Microsoft Store developer account for DigiNote Inc. (company account), and declare the DSA trader status (audit S-25).
2. **Reserve the name.** Apps and games › New product › MSIX or PWA app › reserve "Demios Desktop", or the name you want listed.
3. **Copy the identity values** from the product's *Product management › Product identity* page:

   | Partner Center field | Manifest | Set it as |
   |---|---|---|
   | Package/Identity/Name | `Identity/@Name` | `MSIX_IDENTITY_NAME` |
   | Package/Identity/Publisher (`CN=…`) | `Identity/@Publisher` | `MSIX_PUBLISHER` |
   | Package/Properties/PublisherDisplayName | `Properties/PublisherDisplayName` | `MSIX_PUBLISHER_DISPLAY_NAME` (default `DigiNote Inc.`) |
   | The reserved app name | `DisplayName` (package and tile) | `MSIX_DISPLAY_NAME` (default `Demios Desktop`) |

   - These values identify the product but aren't secret.
   - Keep them out of the repo anyway, so a fork can't build a package that claims our identity. Set them as CI variables, or in the shell when building locally.
   - Copy them **exactly**, or the upload is rejected.
4. **Build the upload package:** `node packaging/msix/build-msix.mjs --store` with the variables set. `--store` refuses the placeholders.
5. **Submit.** Create a submission and upload the `.msix`. Then fill in:
   - pricing and markets (leave out embargoed markets, S-30);
   - the age rating questionnaire (IARC);
   - the privacy policy URL;
   - the listing text ([`docs/store/listing.md`](../../docs/store/listing.md));
   - screenshots from the release build;
   - the certification notes, including the `runFullTrust` reason ([`docs/store/review-notes.md`](../../docs/store/review-notes.md)).

Before the first submission, test the package on a clean Windows VM (audit S-17).
