**Preview build, unsigned.** Demios Desktop {{VERSION}}, from commit {{COMMIT}}. Settings › General › About shows "Preview build (unsigned), commit {{SHORT}}".

These installers are for trying the app early. Please know that:
- **They are not signed or notarized.** Your operating system will warn you before the first launch; see "Opening the app" below.
- **They don't update themselves.** A preview build never checks for updates. To get a newer preview, download it and install it over this one.
- **Reinstall once signed builds arrive.** Signed releases will be able to update themselves; a preview can't move to them on its own. When they are out, install the signed build over the preview.
- **Your chats and settings are kept** when you install a newer preview or a signed build over this one, on every OS (tested on each installer before this release). They live outside the app:
  - macOS: `~/Library/Application Support/io.demios.app`
  - Windows: `%LOCALAPPDATA%\io.demios.app`
  - Linux: `~/.local/share/io.demios.app`

  Uninstalling also keeps them (on Windows, unless you tick "Delete the application data" in the uninstaller). To remove everything, use Settings › General › **Delete all local data** first. Provider keys are in your system keychain, not in that folder.
- Answers come from the AI model you choose and can be wrong. Check important facts.

## Downloads
| OS | File |
|---|---|
| macOS, Apple silicon | `Demios-Desktop-{{VERSION}}-preview-macos-arm64.dmg` |
| macOS, Intel | `Demios-Desktop-{{VERSION}}-preview-macos-x64.dmg` |
| Windows 10/11 (x64) | `Demios-Desktop-{{VERSION}}-preview-windows-x64-setup.exe` |
| Linux (x86_64), any distro | `Demios-Desktop-{{VERSION}}-preview-linux-x86_64.AppImage` |
| Debian, Ubuntu (amd64) | `Demios-Desktop-{{VERSION}}-preview-linux-amd64.deb` |
| Fedora, openSUSE (x86_64) | `Demios-Desktop-{{VERSION}}-preview-linux-x86_64.rpm` |

Check a download against `SHA256SUMS.txt`:
- macOS: `shasum -a 256 -c SHA256SUMS.txt --ignore-missing`
- Linux: `sha256sum -c SHA256SUMS.txt --ignore-missing`
- Windows (PowerShell): `Get-FileHash <file>`, then compare the hash with the file's line.

## Opening the app
**macOS** (11 Big Sur or later)
1. Open the `.dmg` and drag **Demios Desktop** to Applications.
2. Open it once. macOS says it can't check the app for malicious software. Click **Done** (or Cancel).
3. Open **System Settings › Privacy & Security**, scroll down to "Demios Desktop was blocked", click **Open Anyway** and confirm. On macOS 14 and earlier you can also right-click (Control-click) the app › **Open** › **Open**.

Or, in Terminal: `xattr -dr com.apple.quarantine "/Applications/Demios Desktop.app"`

**Windows**
1. Run the `-setup.exe`. It installs for you only and doesn't need an administrator.
2. If SmartScreen says "Windows protected your PC", click **More info › Run anyway**.

The app needs Microsoft Edge WebView2, which Windows 10 and 11 already have. The installer gets it if it's missing.

**Linux**
- **AppImage:** make it executable (`chmod +x Demios-Desktop-*.AppImage`) and run it. If it says FUSE is missing, install `libfuse2` (Ubuntu 24.04: `libfuse2t64`).
- **.deb** (Debian, Ubuntu): `sudo apt install ./Demios-Desktop-{{VERSION}}-preview-linux-amd64.deb`, then start **Demios Desktop** from your apps.
- **.rpm** (Fedora): `sudo dnf install ./Demios-Desktop-{{VERSION}}-preview-linux-x86_64.rpm`. On openSUSE: `sudo zypper install --allow-unsigned-rpm ./Demios-Desktop-{{VERSION}}-preview-linux-x86_64.rpm`. The package is not signed, so zypper needs that flag.
