# Demios for iPhone (early)

The iPhone companion app (ADR-0019): pair with your Demios Node and see what's waiting for you there. The app is SwiftUI over the Rust core `crates/dn-mobile`, through UniFFI.

## What's here now
- **`crates/dn-mobile`**, the Rust core. It handles pairing (the same TLS channel with pinned keys, and the same check numbers as the desktop), the Node's status, the held calls waiting for you (read-only), and unpairing. Its keys live in the iOS Keychain, through a Swift `KeyStore`.
- **`build-xcframework.sh`** builds the core as `build/DemiosCore.xcframework`, for the device and the Simulator (arm64), and writes the Swift bindings to `build/Swift/`.

The SwiftUI app comes once its screens are approved in the design system (PhoneCompanion).

## Build the core

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
```

```bash
apps/ios/build-xcframework.sh release
```

## Not yet
- Approving with Face ID and a Secure Enclave key (ADR-0019 §4): the next step. Until then, answer on your computer.
- Reading held calls away from the Node's network, through the Relay, sealed to the phone (#326).
- The risk badge on each held call (#325).
- Pairing a Node reachable only through a Relay (#315).
- Installing: from Xcode with a free Apple ID until the Apple membership (#301). Push notifications come after the membership (#302).

## Unsafe code
`dn-mobile` is the one crate besides `dn-inference` outside the workspace's `forbid(unsafe_code)`, because UniFFI's generated scaffolding uses `unsafe` (ADR-0009, ADR-0019 §2). It sets `unsafe_code = "deny"`, so hand-written `unsafe` doesn't compile. `scripts/check-unsafe.py` also fails CI on `unsafe` or `allow(unsafe_code)` in its sources.
