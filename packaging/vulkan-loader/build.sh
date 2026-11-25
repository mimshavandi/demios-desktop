#!/usr/bin/env bash
# Builds the Khronos Vulkan loader that the Windows and Linux (AppImage) installers ship next to
# the app (T-INF-24), from a pinned release of https://github.com/KhronosGroup/Vulkan-Loader
# (Apache-2.0). The tag must resolve to the pinned commit; the Vulkan headers it builds against
# are pinned by that commit (scripts/known_good.json, fetched by UPDATE_DEPS).
#
#   packaging/vulkan-loader/build.sh <out dir>
#
# Writes to <out dir>: the loader (vulkan-1.dll on Windows, libvulkan.so.1 on Linux), its
# LICENSE.txt, and SOURCE.txt with the tag, commit and the SHA-256 of the built file (also
# printed, for the release notes). Needs git, cmake, a C/C++ compiler and Python 3.
set -euo pipefail

TAG=v1.4.368
COMMIT=f61c41f6b4c0a2fa3d2b900ab690febbf3235172

out=${1:?usage: build.sh <out dir>}
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$TAG" \
  https://github.com/KhronosGroup/Vulkan-Loader "$work/src"
got=$(git -C "$work/src" rev-parse HEAD)
if [ "$got" != "$COMMIT" ]; then
  echo "Vulkan-Loader $TAG is $got, expected $COMMIT: refusing to build" >&2
  exit 1
fi

cmake -S "$work/src" -B "$work/build" -D UPDATE_DEPS=ON -D BUILD_TESTS=OFF \
  -D CMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "$work/build" --config Release --parallel >/dev/null

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    lib=vulkan-1.dll
    # Multi-config generators (Visual Studio) put it under Release/, Ninja directly in loader/.
    for f in "$work/build/loader/Release/$lib" "$work/build/loader/$lib"; do
      [ -f "$f" ] && { cp "$f" "$out/$lib"; break; }
    done
    [ -f "$out/$lib" ] || { echo "built $lib not found" >&2; exit 1; }
    ;;
  Linux)
    lib=libvulkan.so.1
    cp -L "$work/build/loader/libvulkan.so.1" "$out/$lib"
    ;;
  *)
    echo "only Windows and Linux ship the loader (macOS uses Metal)" >&2
    exit 1
    ;;
esac
cp "$work/src/LICENSE.txt" "$out/LICENSE.txt"

if command -v sha256sum >/dev/null; then
  sha=$(sha256sum "$out/$lib" | cut -d' ' -f1)
else
  sha=$(shasum -a 256 "$out/$lib" | cut -d' ' -f1)
fi
{
  echo "Khronos Vulkan-Loader $TAG ($COMMIT), Apache-2.0"
  echo "built: $lib sha256 $sha"
} | tee "$out/SOURCE.txt"
