# ADR-0019: The iPhone app: a companion for approvals and add-on views, paired with a Node

**Status:** Accepted (owner, 2026-11-26) · **Date:** 2026-11-26 · **Deciders:** project owner, lead · **Extends:** [ADR-0009](0009-security-baseline.md), [ADR-0010](0010-design-system.md), [ADR-0013](0013-plugin-api.md) (§9), [ADR-0014](0014-demios-node.md) (§4a), [ADR-0016](0016-demios-relay.md) · **Issue:** #264

## Context
A Demios Node runs jobs while nobody is at the desk (ADR-0014). Anything risky a job prepares is **held** until a person answers (ADR-0013 §2, ADR-0014 §6). Today, the only place to answer is the desktop app. So a reply prepared at 08:00 waits until the person sits down, and an answer given "on the go" doesn't exist.

The pieces a phone needs are already decided:
- **Pairing and the device group** (ADR-0014 §2). The paired transport is merged (#243): TLS 1.3 in `rustls` with **RFC 7250 raw public keys**, not certificates.
- **Signed answers to held calls**, applied once, with the exact arguments in the signed message (ADR-0014 §6; dn-jobs, #252).
- **Tools that always need a fresh approval,** and **declarative add-on views** drawn by the host on any device (ADR-0013 §9).
- **Reaching the Node** directly on the same network, or through the Relay elsewhere (ADR-0014 §4a).
- **Content-free push wake-ups** through the hosted Relay (ADR-0016 §4.2).

This ADR decides what the phone app is, how it uses the Rust engine, how approvals are protected on the phone, and how it gets onto phones before and after a paid Apple Developer membership.

## Decision

### 1. A companion, not a second desktop
**The app does:**
- **pairing** with a Node by QR code (ADR-0014 §2, and §4a for a Node with no shared network);
- **pending items:** every held call waiting for the person, each with the full review (§4);
- **approve with Face ID, deny, or edit** (editing is an add-on view action that replaces the held call, ADR-0013 §9.2);
- **receipts:** what was approved, when, on which device, and what happened;
- **add-on views** (ADR-0013 §9.3), drawn natively;
- **the Node's status,** including "Your Node has been offline since 03:12" (ADR-0014 §4);
- **notifications** (§6).

**It doesn't:**
- hold chats, projects, artifacts or memory;
- run models;
- change the Node's settings beyond its own (notifications, which device it is, unpairing).

Those stay on the desktop and the Node for now. **The app contains no add-on code.** Views are data drawn by the app, so one app serves every add-on.

### 2. Architecture: a SwiftUI shell over the Rust engine, through UniFFI
- **Rust core (`dn-mobile`):**
  - pairing and the paired transport (`dn-sync`; `rustls` with RFC 7250 and `aws-lc-rs`, as on the desktop);
  - the Relay client;
  - the subset of sync the phone keeps (§3);
  - the local store;
  - building the answer messages.
- **Apple's TLS APIs don't offer RFC 7250 raw public keys**, so the transport must run in Rust. That's the trade-off #243 named.
- **Swift shell:**
  - the UI, in SwiftUI with the design system's tokens (ADR-0010);
  - Face ID and the Secure Enclave (§4), offered to the core as a callback interface (`ApprovalSigner`);
  - the Keychain;
  - push registration;
  - the app lifecycle.
- **The bridge is UniFFI** (Mozilla, MPL-2.0): generated Swift bindings, async functions, and Swift implementations of Rust traits. Options are in the table below. It's a new dependency, approved by the owner on 2026-11-26 with this ADR and listed in `docs/dependencies.md`.
- **The unsafe-code rule (ADR-0009).**
  - UniFFI's generated scaffolding contains `unsafe` blocks, and a crate-level `forbid` also applies to macro-generated code. So **`dn-mobile` becomes the second crate allowed to opt out of `forbid(unsafe_code)`**, after `dn-inference`.
  - It may contain generated bindings only, never hand-written `unsafe`, and a CI check enforces that.
  - This changes ADR-0009's baseline. The owner approved it with this ADR on 2026-11-26, and ADR-0009 points here.
- **Build:** an XCFramework for the device and the Simulator (arm64), built by a script in `apps/ios`. CI builds it on a macOS runner and runs the core's tests on the host.

### 3. What the phone keeps: a subset, sealed to the phone
- The phone is a member of the device group (ADR-0014 §2), but **it doesn't receive the group key**, so it can't read chats, projects or memory even if they reached it.
- The Node **seals what the phone needs to the phone's own X25519 key** (HPKE, ADR-0016's suite):
  - held calls and their status;
  - receipts;
  - view documents;
  - the device list;
  - the Node's status;
  - notifications.

  These go over the paired channel, or to the phone's restricted Relay mailbox.
- **Answers go the other way,** signed (§4).
- The phone needs **no snapshot** (ADR-0014 §4a): it asks the Node for the current list of pending items when it connects.
- **Local store:** SQLite in the app's container, under iOS Data Protection class *Complete* (readable only while the phone is unlocked). It holds the items above and nothing else.
- **Unpairing** on the phone deletes the local store and the keys. Removing the phone from another device revokes its keys and closes its mailbox (ADR-0014 §8).
- The subset member is new in `docs/design/dn-sync.md` and is an action item.

### 4. Approvals: the full review, then Face ID
- **The review shows everything the call will do:**
  - the tool and the add-on and job that asked;
  - **every argument**: for a message, To, CC, BCC, the subject, the whole body and the attachments ("none" when there are none);
  - a **diff** against the version the person saw before (ADR-0013 §9.2);
  - a "New recipient" mark for anyone not already in the thread, and every link.

  Long content scrolls, and it's never truncated in a way that hides parts.
- **Two keys on the phone:**
  - **Transport identity:** Ed25519 and X25519 keys for the paired channel and sealing (ADR-0014 §2). They're kept in the Keychain as this-device-only items that never sync to iCloud.
  - **Approval key:** a **P-256 key generated in the Secure Enclave** (the Secure Enclave supports only P-256). It's created with an access control that requires the current Face ID enrolment for **every** signature. The private key never leaves the Secure Enclave and can't be exported.
  - The public approval key is registered at pairing, as a **presence-bound key** (ADR-0013 §9.4), and the person confirms it on the pairing screen.
- **Approve = sign with Face ID.**
  - The message is the held answer message (`dn-held-answer v1`: approval id, tool, approve, SHA-256 of the exact stored arguments), signed with ECDSA P-256.
  - The Node checks it with `aws-lc-rs` (already a dependency) against the phone's registered approval key, then runs the call once (dn-jobs).
  - For `approval_required` tools, this is the one-time token (ADR-0013 §9.4).
- **Deny needs no Face ID.** A forged "deny" can't make anything happen.
- **If Face ID enrolment changes** (a face added or reset), the Secure Enclave key stops working on purpose. The app says so, and the phone is paired again to register a new approval key.
- **No passcode fallback for approvals in the app.** iOS offers the passcode after failed Face ID attempts, as the system does for every app. A phone without Face ID (passcode only) can still approve, and the pairing screen shows "approvals use your passcode, not Face ID". The Node setting that accepts only presence-bound keys (ADR-0013 §9.4) can refuse such a phone.

### 5. Reaching the Node
- **The defaults of ADR-0014 §4a apply:**
  - directly on the same network;
  - through the Relay elsewhere, with a long-poll while the app is open;
  - an opened path only if the person chose one on the Node.
- **The phone never listens for connections.**

### 6. Notifications
- **Push** (after a paid Apple Developer membership, §7): the Node asks the hosted Relay to send a **content-free alert** through APNs ("Something is waiting in Demios"), as ADR-0016 §4.2 describes.
  - Tapping it opens the app, which fetches and decrypts.
  - Push is opt-in per phone, and the opt-in screen says what the Relay and Apple see (that a wake-up was sent, and when).
  - A self-hosted Relay can't send official app pushes.
  - Showing message content on the lock screen (a notification service extension that decrypts) isn't part of this ADR.
- **Without push** (before the membership, or when turned off): pending items appear when the app is opened, and while it's open. The home screen says "Notifications are off. Open the app to see what's waiting".

### 7. Getting the app onto phones
**Before a paid Apple Developer membership:**
- A **development install from Xcode with a free Apple ID**, on the maintainer's own iPhone.
- Apple limits this:
  - the app stops launching after 7 days and must be installed again;
  - push notifications aren't available;
  - a few apps per device.
- This build configuration carries no push entitlement.

**After the membership:** **TestFlight** for testers, with push.

**The App Store** comes later, with its own checklist: review, privacy labels, export compliance for the encryption.

**No Android app for now.** The Rust core and UniFFI keep Kotlin bindings possible, and an Android app would get its own ADR.

### 8. Demo mode, for test builds only
- **What it is:** a **fake Node inside the app**, with synthetic data (pending items, views, receipts). It lets the app be tested without a Node, a Relay or real accounts.
- **It exists only in a separate build configuration** (`Demo`), behind a Rust feature and a Swift compile flag.
- **Release and TestFlight builds can't contain it.** CI fails if the release configuration enables the flag or the feature, or if the release binary contains the demo data's marker strings.
- **What it's for:** automated testers. That includes AI agents that use the app on iOS Simulators like new users and report broken flows, layout, translation and accessibility issues (for example the open-source mobile-agent-testers).
  - Those runners are free only for public repositories, so builds that include anything not public are tested from a separate public repository that receives **only the Simulator build of the demo configuration**: no source, keys or real data.
  - Face ID is simulated in the Simulator. The real Face ID path is checked by hand on a device before each TestFlight release.

### 9. Security and privacy, in short
- **The phone holds** only what §3 lists, under Data Protection *Complete*, and the app hides its content in the app switcher.
- **No analytics or crash reporting** leaves the phone (ADR-0009, no telemetry). A crash report is something the person chooses to send, as on the desktop.
- **A jailbroken phone isn't detected.** Choosing where approvals happen is choosing whom to trust, as for the Node (ADR-0014 §7).
- **A lost phone:**
  - it can't approve without the person's face;
  - removing it from any other device revokes its keys;
  - its local copy is readable only by someone who can unlock it.

## Options considered

### How the app uses the Rust engine
| Option | What it is | For | Against |
|---|---|---|---|
| **A. SwiftUI + UniFFI (chosen)** | Generated Swift bindings to a Rust library | Mature and maintained by Mozilla (releases in 2026, used in Firefox for iOS); async functions; Swift can implement Rust traits (the Secure Enclave signer); Kotlin bindings for a later Android app | A new dependency and a code generator in the build; generated `unsafe` (an opt-out like `dn-inference`); its Swift 6 strict-concurrency support is partial |
| B. SwiftUI + swift-bridge | Rust and Swift share types through generated glue | Fewer copies across the boundary | Pre-1.0 (0.1.x) and mostly one maintainer; Swift only, so Android would need a second bridge |
| C. Tauri 2 mobile | The desktop's web UI in a webview on iOS | Reuses the TypeScript UI and design system; an official biometric plugin | Not a native shell; Secure Enclave signing and push still need native plugins; a webview for the screen where people approve risky actions; heavier |
| D. Hand-written C ABI (cbindgen) | `extern "C"` functions and a Swift wrapper | No generator | Hand-written `unsafe` across the whole boundary, which ADR-0009 forbids |
| E. Crux-style shared core | An architecture over UniFFI where Rust owns the app logic | Testable app logic in Rust | More structure than a companion needs; can be adopted later on top of A |

### Where the approval key lives
| Option | Bound to the person | Notes |
|---|---|---|
| **P-256 in the Secure Enclave, Face ID for every signature (chosen)** | Yes, in hardware | The Secure Enclave supports only P-256, so the Node verifies ECDSA P-256 for this key |
| Ed25519 in the Keychain, behind Face ID with LocalAuthentication | Only by the app's own check | The key is in software; a compromised app could sign without the face |
| The transport key signs approvals | No | Any code with the device key could approve |

### Notifications
| Option | Notes |
|---|---|
| **Content-free APNs alerts through the hosted Relay (chosen, after the membership)** | The only way for the official app; nothing about the item is sent |
| No push (chosen until the membership) | Works; people must open the app |
| Content in the notification (a decrypting service extension) | Better at a glance, but content on the lock screen; a later decision |
| A messaging-app bot | Routes private content through a third party; not considered |

## Trade-offs
- **A companion, not a full client,** keeps the phone's data small and its attack surface narrow. People who want chats on the phone will have to wait for another ADR.
- **The phone doesn't get the group key.** That means the Node does extra sealing work, in exchange for a lost phone not holding the person's chats.
- **Face ID on every approval** adds a second of friction exactly where it belongs, and none on reading or denying.
- **Free-Apple-ID installs** expire every 7 days and can't push. This is a stopgap that the membership replaces, not a distribution method.
- **A second crate that opts out of the unsafe-code lint** is a real exception. It's limited to generated bindings and checked in CI.

## Check before building (may have changed since this was written)
- Free (personal team) provisioning: that Secure Enclave keys with a biometric access control and Face ID work in a development install. They're expected to: only push, iCloud and a few others are restricted.
- Building `aws-lc-rs` for iOS device and Simulator targets (toolchain requirements), and `rustls`'s RFC 7250 support on those targets.
- UniFFI's current release: async with the app's runtime, callback interfaces, and the Swift 6 concurrency status.

## Consequences
- **New code:**
  - `apps/ios`: the SwiftUI app, the XCFramework build script, and the `Demo` configuration;
  - `crates/dn-mobile`: the UniFFI surface over `dn-sync`, the Relay client and the subset store.
- **`dn-sync` and its design note:** the subset member (§3), and approval keys that are P-256 and presence-bound.
- **dn-jobs (#252):** its `AnswerVerifier` on the Node accepts ECDSA P-256 answers from a device's registered approval key.
- **ADR-0009:** a second, generated-only exception to the unsafe-code lint.
- **`docs/dependencies.md`:** `uniffi` is listed (owner approved 2026-11-26), and added to the workspace when `dn-mobile` first uses it.
- **Design:** the phone screens are designed in the design reference first (ADR-0010; design standard §8). They are pairing, pending items and the review, Face ID approval, receipts, views, status, and the notification opt-in.

## Action items (to be filed as issues)
1. `dn-mobile` crate with UniFFI (the unsafe-code exception with its CI check), and the XCFramework build.
2. The SwiftUI app: pairing, pending items and review, approve with Face ID, deny, receipts, views, status.
3. Secure Enclave approval key and presence-bound registration at pairing; P-256 verification in the Node's `AnswerVerifier`.
4. The subset member in `dn-sync`: sealing to the phone's key, no group key, and the design note.
5. The `Demo` configuration with a fake Node, the CI check that keeps it out of release builds, and the public test repository that receives only the Simulator build.
6. After the membership: the push entitlement, APNs registration through the hosted Relay, TestFlight.
