#!/usr/bin/env bash
# Builds dn-mobile (the iPhone app's Rust core, ADR-0019 §2) as an XCFramework for the
# device and the Simulator (arm64), with its UniFFI Swift bindings.
#
#   apps/ios/build-xcframework.sh [debug|release]
#
# Output (not committed): apps/ios/build/DemiosCore.xcframework and
# apps/ios/build/Swift/dn_mobile.swift. Needs Xcode and the Rust targets
# aarch64-apple-ios and aarch64-apple-ios-sim (`rustup target add ...`).
set -euo pipefail

profile="${1:-release}"
case "$profile" in
  release) flag="--release" ;;
  debug) flag="" ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/apps/ios/build"
targets=(aarch64-apple-ios aarch64-apple-ios-sim)

cd "$root"
for t in "${targets[@]}"; do
  if ! rustup target list --installed | grep -qx "$t"; then
    echo "Missing Rust target $t. Run: rustup target add $t" >&2
    exit 1
  fi
done

export IPHONEOS_DEPLOYMENT_TARGET=17.0
for t in "${targets[@]}"; do
  cargo build -p dn-mobile --lib $flag --target "$t"
done

rm -rf "$out"
mkdir -p "$out/Swift" "$out/headers"
device_lib="$root/target/aarch64-apple-ios/$profile/libdn_mobile.a"
sim_lib="$root/target/aarch64-apple-ios-sim/$profile/libdn_mobile.a"

# The bindings come from the built library's metadata (UniFFI library mode).
cargo run -q -p dn-mobile --features bindgen --bin uniffi-bindgen -- \
  generate --library "$device_lib" --language swift --out-dir "$out/Swift"

mv "$out/Swift/"*.h "$out/headers/"
# Clang module map, named as the XCFramework expects.
cat "$out/Swift/"*.modulemap > "$out/headers/module.modulemap"
rm "$out/Swift/"*.modulemap

xcodebuild -create-xcframework \
  -library "$device_lib" -headers "$out/headers" \
  -library "$sim_lib" -headers "$out/headers" \
  -output "$out/DemiosCore.xcframework"

echo "Built $out/DemiosCore.xcframework and $out/Swift/"
