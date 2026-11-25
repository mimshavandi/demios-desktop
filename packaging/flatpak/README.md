# Flatpak packaging (io.demios.app)

Files for building Demios Desktop as a Flatpak and, later, publishing it on Flathub. Nothing here has been submitted to Flathub. Submitting is the owner's step (see [Submitting to Flathub](#submitting-to-flathub)).

| File | What it is |
|---|---|
| `io.demios.app.yml` | The manifest. It uses the GNOME 49 runtime (WebKitGTK 4.1 for Tauri) and the rust-stable, node22 and llvm20 SDK extensions. Each `finish-args` permission has its reason in a comment |
| `io.demios.app.metainfo.xml` | AppStream metainfo: the listing text from [docs/store/listing.md](../../docs/store/listing.md), OARS rating, screenshots and releases |
| `io.demios.app.desktop` | The desktop entry. It also registers the `demios://` link scheme |
| `generate-sources.sh` | Writes `generated/`, the offline cargo and npm source lists (flatpak-builder-tools at a pinned commit). The output is not committed |

## What is different in the Flatpak build

- **Tool servers (MCP): remote servers only.** The build uses the app's `flatpak` cargo feature, and the app also checks `FLATPAK_ID` at run time. Settings › Models › Tool servers then offers only remote (HTTPS) servers, with a line saying why. A local server saved earlier fails to start with the same message. The sandbox can't run programs on the host without `org.freedesktop.Flatpak`, which is a full sandbox escape, so the manifest doesn't ask for it. Bundled servers (shipped inside the Flatpak under `/app`) are allowed by this decision, but none are bundled yet. The first one needs a follow-up that allows stdio commands under `/app` only.
- **Keys use the keyring through D-Bus.** The keyring crate speaks the Secret Service API (`org.freedesktop.secrets`), not the Secret portal, so the manifest asks for `--talk-name=org.freedesktop.secrets`. This works with GNOME Keyring and with KWallet when its Secret Service support is on. When no keyring daemon is running, saving a key fails, and the app's existing "keychain unavailable" message is shown. Supporting the Secret portal needs a new dependency, so it would be a separate task with the lead's approval.
- **Data stays in the sandbox.** Chats, settings, the tool ledger and downloaded models live under `~/.var/app/io.demios.app/data/io.demios.app/` (Flatpak sets `XDG_DATA_HOME`). Delete all local data clears that folder. Uninstalling with `flatpak uninstall --delete-data io.demios.app` also removes it.
- **The Hugging Face cache is the app's own.** Inside the sandbox `XDG_CACHE_HOME` is `~/.var/app/io.demios.app/cache`, so the app reads `…/cache/huggingface/hub`, not `~/.cache/huggingface/hub`. Models already in the host cache don't appear unless the user grants read-only access themselves:

  ```bash
  flatpak override --user --filesystem=~/.cache/huggingface:ro --env=HF_HUB_CACHE=$HOME/.cache/huggingface/hub io.demios.app
  ```

  The app only reads that cache and never writes to it, so `:ro` is enough. The manifest doesn't grant this by default, to keep the home folder out of the sandbox.
- **Local models run on the CPU.** The build uses dn-inference's default features. Vulkan needs `glslc` at build time, and the SDK doesn't ship it. CUDA isn't possible on Flathub.
- **Links and sign-in pages** open in the user's browser through the OpenURI portal (`xdg-open` inside the sandbox), so this needs no permission.
- **No self-updater.** Flathub updates the app.

Status: the keyring, the Hugging Face cache and the portals are described from the code and Flatpak's documented behaviour. They have not been checked in a running Flatpak yet, because the CI job only builds and lints. Check them by hand on a Linux desktop with the CI bundle before the first submission:

1. Save a provider key, restart the app, and confirm the key is still there.
2. Open a link from an answer.
3. Sign in to an OAuth MCP server.
4. Open a Hugging Face model with and without the override above.

## Build locally (Linux)

```bash
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
```

```bash
flatpak install --user -y flathub org.flatpak.Builder
```

```bash
packaging/flatpak/generate-sources.sh
```

```bash
cd packaging/flatpak && flatpak run org.flatpak.Builder --user --install --force-clean --install-deps-from=flathub build-dir io.demios.app.yml
```

```bash
flatpak run io.demios.app
```

`generate-sources.sh` needs `git`, `python3`, `curl` and network access. Run it again whenever `Cargo.lock` or a `package-lock.json` changes. The build itself is offline. On macOS you can run the generator, but not the build.

Linters, as Flathub runs them:

```bash
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest packaging/flatpak/io.demios.app.yml
```

```bash
flatpak run --command=appstreamcli org.flatpak.Builder validate packaging/flatpak/io.demios.app.metainfo.xml
```

CI does the same in [`.github/workflows/flatpak.yml`](../../.github/workflows/flatpak.yml). It runs on manual dispatch and on changes under `packaging/flatpak/`, it is not a required check, and it uploads the `.flatpak` bundle.

## Before each release

- Add a `<release>` to the metainfo for the new version (newest first, with date and `type="stable"`), using the CHANGELOG section as its description.
- Keep the metainfo `<description>` in step with `docs/store/listing.md` and `tauri.conf.json`.

## Submitting to Flathub

These steps are the owner's. Nothing here does them.

1. **Release build screenshots.** Replace the development screenshots in the metainfo with captures from a release build (docs/store/listing.md, S-24). Use stable URLs: a tagged commit, not `main`.
2. **Confirm the listing text, the OARS answers and the developer name** in the metainfo.
3. **A Flathub account.** Sign in at flathub.org with the GitHub account that will maintain the app.
4. **The submission PR.** Fork `flathub/flathub`, branch from `new-pr`, and add:
   - `io.demios.app.yml`, with the `type: dir` source swapped for the release:
     ```yaml
     - type: git
       url: https://github.com/mimshavandi/demios-desktop.git
       tag: v0.1.0
       commit: <full sha of the tag>
     ```
   - the files `generate-sources.sh` produced for that tag (`generated/*.json`), with the manifest's `sources` paths adjusted to point at them.
   - in the PR description, one reason per `finish-args` permission (the manifest comments) and the MCP note: local programs can't be started in this build; remote servers only.

   Then open the PR against `new-pr`. A reviewer runs a test build (`bot, build`). After approval, Flathub creates `flathub/io.demios.app` and the owner maintains it from then on.
5. **Domain verification for io.demios.app.** The app id is under `demios.io`, so the verified badge needs control of that domain. In the Flathub developer portal, open the app's verification page. It gives a token. Serve that token at `https://demios.io/.well-known/org.flathub.VerifiedApps.txt`, then press Verify. Until then the app is listed as unverified.
6. **Updates.** For each release, open a PR on `flathub/io.demios.app` with the new tag and commit and the regenerated source lists. Flathub's bot can do part of this through `x-checker-data`, which isn't set up here.
