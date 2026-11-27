# Demios for iPhone (early)

The iPhone companion app (ADR-0019): pair with your Demios Node and see what's waiting for you there. The app is SwiftUI over the Rust core `crates/dn-mobile`, through UniFFI.

## What's here now
- **`crates/dn-mobile`**, the Rust core. It handles pairing (the same TLS channel with pinned keys, and the same check numbers as the desktop), the Node's status, the held calls waiting for you (read-only), and unpairing. Its keys live in the iOS Keychain, through a Swift `KeyStore`.
- **`build-xcframework.sh`** builds the core as `build/DemiosCore.xcframework`, for the device and the Simulator (arm64), and writes the Swift bindings to `build/Swift/`.

- **`Demios.xcodeproj`** and **`Demios/`**, the SwiftUI app, read-only for now. Its screens follow the design system's PhoneCompanion, approved by the owner on 27 Nov:
  - **Welcome**, then **Pair**: scan the code your Node shows (or paste the link), compare the check numbers, and confirm on the Node.
  - **Waiting**: the held calls on the Node, oldest first, with loading, empty and offline states. When the Node doesn't answer, the last list stays, marked with its time.
  - **A held call**: every argument in full. There's no Approve or Deny yet.
  - **This phone**: the Node, this phone's name there, and **Unpair this phone**.

  Its keys stay in the Keychain, on this device only (`WhenUnlockedThisDeviceOnly`, no iCloud). Its store is readable only while the phone is unlocked, and the app switcher shows no content.

## Build the core

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
```

```bash
apps/ios/build-xcframework.sh release
```

## Run the app

Build the core first (above, `debug` is enough), then open `apps/ios/Demios.xcodeproj` in Xcode and run the **Demios** scheme on a Simulator or your iPhone. On an iPhone, choose your team under Signing (a free Apple ID works). If Xcode says the bundle ID `io.demios.phone` is taken, change it to your own.

To pair, run `dn node pair` on the Node and scan the code, or paste the link. The phone has to be on the Node's network.

Debug builds take launch arguments for trying the screens without a Node; release builds don't have them:
- `-DemiosDemo ready|empty|offline|unpaired` uses made-up data in place of the core;
- `-DemiosDemoScreen pair|call|phone` opens that screen;
- `-DemiosPairLink <link>` pastes a link and connects with the real core, for a Simulator, which has no camera.

## Not yet
- Approving with Face ID and a Secure Enclave key (ADR-0019 §4): the next step. Until then, answer on your computer.
- Reading held calls away from the Node's network, through the Relay, sealed to the phone (#326).
- The risk badge on each held call (#325).
- Pairing a Node reachable only through a Relay (#315).
- Installing: from Xcode with a free Apple ID until the Apple membership (#301). Push notifications come after the membership (#302).

## Unsafe code
`dn-mobile` is the one crate besides `dn-inference` outside the workspace's `forbid(unsafe_code)`, because UniFFI's generated scaffolding uses `unsafe` (ADR-0009, ADR-0019 §2). It sets `unsafe_code = "deny"`, so hand-written `unsafe` doesn't compile. `scripts/check-unsafe.py` also fails CI on `unsafe` or `allow(unsafe_code)` in its sources.
