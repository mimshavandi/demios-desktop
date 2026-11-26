# ADR-0017: Licence: PolyForm Shield 1.0.0 for the app, `dn`, the Node and the shared crates

**Status:** Accepted (owner, 2026-11-26) · **Date:** 2026-11-26 · **Deciders:** project owner, lead · **Supersedes:** the licence named in [ADR-0001](0001-repo-and-crate-layout.md) (Apache-2.0) · **Affects:** [ADR-0015](0015-encrypted-sharing-and-forms.md), [ADR-0016](0016-demios-relay.md)

## Context
This repository was published under Apache-2.0. Apache-2.0 lets anyone fork the app, rebrand it and sell it, or build a competing product or hosted service on it. The owner decided that nobody may sell Demios or compete with it using its code, and that the restriction must not lapse over time.

## Decision
- **Licence:** everything in this repository that is ours (the desktop app, `dn`, the Node, the shared crates and the share viewer) is licensed under the **PolyForm Shield License 1.0.0**, written in package metadata as `LicenseRef-PolyForm-Shield-1.0.0` (it isn't on the SPDX licence list; the Flatpak metainfo uses AppStream's `LicenseRef-proprietary=` form with the licence URL), from the commit that adds this ADR.
  - Anyone may read, build, change, share and use the code **for any purpose except providing a product that competes** with the software or with any product DigiNote, Inc. provides using it. That includes competing products that are free.
  - **It never converts** to an open-source licence.
- **`LICENSE`** carries the licence text with a `Required Notice:` line (copyright DigiNote, Inc.) and a `Licensor Line of Business:` line, so competing with a Demios product line stays excluded even if a product is paused.
- **Earlier versions stay under Apache-2.0.** Everything published up to and including `v0.1.0-preview.8cd482c` (commit 8cd482c) was released under Apache-2.0, and that can't be withdrawn.
- **Contributions:** contributors license their contributions to DigiNote, Inc. under Apache-2.0 and sign off their commits (Developer Certificate of Origin); DigiNote, Inc. publishes them under the project's licence (CONTRIBUTING.md). Until now every commit has come from the owner, so no consent from others was needed for this change.
- **Third-party dependencies keep their licences.** All are permissive (deny.toml), which is compatible with distributing under PolyForm Shield. Our own crates are marked `publish = false`; cargo-deny and cargo-about skip them as our own code (`[licenses.private]`, `private.ignore`), so the third-party licence list in the app shows only other packages, and no dependency can come in under this licence unnoticed.
- **Wording:** the app and its docs say "source-available", not "open source".
- **The Relay, client and server, is part of this repository** and under this licence (ADR-0016 §7).
- **Other licences, unchanged by this ADR:** the Twin and the organisation layer stay proprietary.

## Options considered
| Option | Licence | Effect |
|---|---|---|
| A | Keep Apache-2.0 | Anyone may sell forks or compete. Most trust and adoption |
| B | FSL-1.1-Apache-2.0 | No competing use for two years per release, then Apache-2.0 |
| **C (chosen)** | **PolyForm Shield 1.0.0** | **No competing use, permanently.** A standard, lawyer-written licence; chosen over n8n's Sustainable Use License, which is n8n-specific text |

## Trade-offs
- **Not open source** by the OSI definition. Linux distribution repositories that require open-source licences won't carry it. Our own installers, Homebrew casks and Flathub (marked as not open source) still can, so the current distribution through our own installers is unaffected.
- **Fewer outside contributors** than Apache-2.0 would attract.
- **It protects the code, not the idea.** A product written from scratch can still compete; the brand needs a registered trademark, and paid features need their own licence terms.
- **A lawyer should review** the licence set, the contribution terms and the paid-feature terms before the first sale.

## Consequences
- `LICENSE`, `Cargo.toml`, the npm packages, `tauri.conf.json`, the Flatpak metainfo, README, CONTRIBUTING, the privacy policy and the store listing name the new licence.
- The CHANGELOG lists the change for users.
- ADR-0015 and ADR-0016 name this licence for the parts that live in this repository; the Relay server is `dn relay run` here, not a separate repository.

## Action items
1. demios.io: replace "open source" with "source-available" and name the licence.
2. Lawyer review of the licence set, contribution terms and paid-feature terms (owner).
3. Register the "Demios" trademark in the EU and US (owner).
