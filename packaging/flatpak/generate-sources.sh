#!/usr/bin/env bash
# Generates the offline source lists the Flatpak manifest reads (packaging/flatpak/generated/):
# every cargo crate in Cargo.lock, every npm package in the two package-lock.json files, and
# cargo-about (the licence-list tool) with its own crates. Flathub builds have no network, so
# flatpak-builder downloads all of these up front and checks each against its sha256.
#
# The output is not committed (it is large and changes with every lockfile change): CI runs
# this script, and a Flathub submission commits its output to the flathub/io.demios.app repo.
#
# Needs: git, python3 (3.9+), curl, and network access to GitHub, crates.io and PyPI.
# Usage: packaging/flatpak/generate-sources.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/generated"

# flatpak-builder-tools, pinned to a commit (update deliberately, then regenerate).
TOOLS_REPO=https://github.com/flatpak/flatpak-builder-tools.git
TOOLS_COMMIT=9d86d336a39b9af38b86f114b6ce5820fe260f91
# Keep in step with apps/desktop/scripts/licenses.mjs and docs/dependencies.md.
CARGO_ABOUT_VERSION=0.9.3
CARGO_ABOUT_SHA256=f2309e4e7cd6fbb25c4c8d68e69c728936dec58a0cb7940dcb382d5be32aa52b

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "flatpak-builder-tools @ $TOOLS_COMMIT"
git -C "$work" init -q tools
git -C "$work/tools" fetch -q --depth 1 "$TOOLS_REPO" "$TOOLS_COMMIT"
git -C "$work/tools" checkout -q FETCH_HEAD

python3 -m venv "$work/venv"
"$work/venv/bin/pip" install -q --disable-pip-version-check aiohttp==3.14.5 tomlkit==0.15.3 "$work/tools/node"
py="$work/venv/bin/python"

mkdir -p "$out"

echo "cargo sources (Cargo.lock)"
"$py" "$work/tools/cargo/flatpak-cargo-generator.py" "$repo/Cargo.lock" -o "$out/cargo-sources.json"

echo "npm sources (apps/desktop, ui/artifact-sandbox)"
"$work/venv/bin/flatpak-node-generator" npm "$repo/apps/desktop/package-lock.json" -o "$out/node-sources-app.json"
"$work/venv/bin/flatpak-node-generator" npm "$repo/ui/artifact-sandbox/package-lock.json" -o "$out/node-sources-sandbox.json"

echo "cargo-about $CARGO_ABOUT_VERSION"
url="https://static.crates.io/crates/cargo-about/cargo-about-$CARGO_ABOUT_VERSION.crate"
curl -sSfL "$url" -o "$work/cargo-about.crate"
got="$("$py" -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$work/cargo-about.crate")"
if [ "$got" != "$CARGO_ABOUT_SHA256" ]; then
  echo "cargo-about crate checksum mismatch: $got" >&2
  exit 1
fi
tar -xzf "$work/cargo-about.crate" -C "$work"
"$py" "$work/tools/cargo/flatpak-cargo-generator.py" "$work/cargo-about-$CARGO_ABOUT_VERSION/Cargo.lock" -o "$out/cargo-about-sources.json"
cat > "$out/cargo-about.json" <<EOF
[
  {
    "type": "archive",
    "archive-type": "tar-gzip",
    "url": "$url",
    "sha256": "$CARGO_ABOUT_SHA256"
  }
]
EOF

echo "done: $out"
ls -lh "$out"
