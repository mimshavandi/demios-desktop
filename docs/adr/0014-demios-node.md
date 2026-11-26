# ADR-0014: Demios Node: the engine run headless on the user's own machine or server, with paired clients

**Status:** Accepted (owner, 2026-11-26), amendment proposed (§4a, owner approval needed) · **Date:** 2026-11-25 (amended 2026-11-26: offline notice; amendment proposed 2026-11-26: §4a remote access) · **Deciders:** project owner, lead · **Extends:** [ADR-0001](0001-repo-and-crate-layout.md), [ADR-0006](0006-tools-mcp-capabilities.md), [ADR-0007](0007-local-storage-and-secrets.md), [ADR-0009](0009-security-baseline.md), [ADR-0013](0013-plugin-api.md), [ADR-0016](0016-demios-relay.md) · **Issue:** #203

## Context
Everything in Demios runs inside the desktop app today. When the app is closed or the computer sleeps, nothing runs:
- an add-on's background jobs (ADR-0013) stop;
- a phone has nothing to talk to;
- work that should happen overnight, such as a daily summary, waits until the user opens the app.

The owner decided that always-on work runs on a **Demios Node**: the same Rust engine, run headless as a service.
- **For now, a Node runs only on hardware the user controls:** their own computer (a home PC, a Mac mini), a NAS, or a VPS they rent.
- **A Node hosted by Demios is deferred.** It isn't part of this ADR.
- **The desktop app, and later a phone app, are clients** that pair with the Node.

This ADR decides the Node's shape: pairing, sync, models, approvals, secrets, operation and installation. It doesn't implement it.

**Be honest about what a Node is.** A Node holds the user's data **decrypted**: chats, memory, settings, provider keys and tool tokens. It has to, because it runs models and tools on that data while no client is awake. "End-to-end encrypted" in this ADR means that **nothing between the devices and the Node** can read it: not the network, not a relay, not Demios. It does **not** mean the Node's own disk, memory or administrator can't read it. Whoever controls the Node's machine controls the data. On a VPS, that includes the hosting provider.

## Decision

### 1. One engine, three ways to run it
- **The Node is the `dn` binary in service mode** (`dn node run`), built from the same crates as the app: `dn-agent`, `dn-tools`, `dn-store`, `dn-inference`, `dn-providers`, `dn-knowledge`, `dn-sync`, `dn-secrets`.
  - It has no UI. Clients configure it.
  - It's open source, like the rest of `dn`. An add-on build of the Node (ADR-0013 §1) is a separate build target, as it is for the app.
- **The desktop app keeps working with no Node.** Pairing a Node is optional, and nothing in the app needs one, except work that must run while the app is closed.
- **One data set, several runtimes.** The desktop app, the Node and later a phone app each keep a full local copy of the user's data and sync it (§3). The Node isn't a server that the others query; it's a peer that never sleeps.

### 2. Pairing and authentication between devices
- **Every device has its own long-term key pair** (X25519 for key agreement, Ed25519 for signatures), created on first start and kept in that device's secret store (§7). The private key never leaves the device.
- **The Node keeps the list of paired devices.** Every paired device sees the list too. That's the **device group**.
- **Pairing a new device:**
  1. On the Node (`dn node pair`) or on an already paired device, the user starts pairing. That shows a **QR code**, or the same thing as a link to copy. It holds the Node's address, the Node's public key, and a one-time secret of 128 bits that expires after 10 minutes.
  2. The new device connects to that address, checks the Node's public key against the one in the code, and proves it knows the one-time secret.
  3. Both sides then show the same **short check phrase** (a hash of both public keys), and the user confirms on the Node side, or on the device that started the pairing.
  4. The Node adds the device's public key to the group, and the one-time secret is used up.
- **Authentication after pairing:** every connection is mutually authenticated with the two long-term keys. There are no passwords or bearer tokens, and no account.
- **Removing a device** is possible from any paired device, after a confirmation. The Node drops the key at once, and the removed device can't connect again. Its local copy isn't wiped, because a removed device may be stolen and offline. The confirmation says so, and offers "Delete all" (§8) on that device if it's in reach.
- **Transport:** TLS 1.3 with `rustls`, already in the dependency tree. Both sides present self-signed certificates whose keys are the paired keys, and each side pins the other's key, so no certificate authority is involved. The Noise protocol is the alternative (options table).

### 3. End-to-end encrypted sync of chats, memory and settings
- **What syncs:**
  - chats with their artifacts;
  - projects and their files;
  - add-on memory (ADR-0013 §5);
  - settings, except per-device ones such as window size or which local model is loaded;
  - the held-approvals queue (§6).
- **What doesn't sync:**
  - downloaded models, which are large and per machine; each runtime has its own;
  - the knowledge index, which each runtime rebuilds from the synced files;
  - secrets (§7), which are synced only when the user asks for it per key;
  - logs.
- **Encryption:**
  - In transit, every sync message travels over the paired TLS channel (§2).
  - Records are also **sealed per device group** with a symmetric group key, so the Relay (§4) or a backup of the sync log sees only ciphertext.
  - The group key is rotated when a device is removed.
- **Model:** each device appends changes to its own log. Records carry a hybrid logical clock, and a field's last write wins.
  - **Deletes are tombstones,** so deleting a chat on one device deletes it everywhere. A purge also removes the data behind it on every runtime (§8).
  - Text that two devices change at once keeps both versions and shows a conflict. It never silently picks one.
  - The details (record format, clock, compaction) go into a `dn-sync` design note before implementation.

### 4. The Node never needs Demios servers
- **Pairing, sync, models, tools and approvals all work with no Demios server.** Devices connect to the Node directly.
- **How a client reaches the Node is the user's choice**, shown in the pairing screen:
  - the same local network, found by mDNS or by entering an address;
  - the user's own VPN or overlay network, such as WireGuard or Tailscale;
  - a port the user forwards, or their own reverse proxy. TLS is still end to end with the pinned keys, so a proxy that terminates TLS breaks pairing on purpose.
- **The Demios Relay is optional** (ADR-0016). It's an encrypted store-and-forward mailbox service: when two devices aren't online at the same time, a device delivers its sealed changes, approvals or notifications to the other's mailbox there. It holds ciphertext only (§3), never decrypts and holds no keys. ADR-0016 states what it can still see: mailbox ids, timing and sizes.
  - **A Node can run its own Relay** (ADR-0016 §7), so even store-and-forward needs no Demios server.
  - **Without any Relay,** devices sync only when they reach the Node directly, and Settings › Node says so.
- **Push to a phone is the one exception the platforms impose.** iOS and Android deliver background notifications only through Apple's and Google's push services, which need a sender that holds the app's push credentials. So:
  - **Without push, nothing breaks.** Approvals wait until a device connects. The desktop app keeps a live connection while it runs.
  - **Push is opt-in per device.** The Node can send a **content-free** wake-up ("1 approval waiting", nothing about what it is) through either:
    - the Demios-hosted Relay (ADR-0016 §4.2), which sends the fixed wake-up and sees the device's push token. Official app push can only come from there, because it needs the app's push credentials; or
    - the user's own push server (UnifiedPush or ntfy on Android), from the Node or the user's own Relay.

    The device then connects to the Node directly to read and answer.
  - The pairing screen says which of these is on.
- **Offline notice for the Node (optional, off by default).** So the user learns that jobs aren't running before a missed result shows it:
  - while it runs, the Node puts a small sealed "alive" record in the user's own Relay mailbox at a fixed interval (for example every 15 minutes);
  - a paired phone or desktop that sees no fresh record for a set time says "Your Node has been offline since 03:12" (a local notification on the device; the Relay never sends it and can't read the record);
  - **the cost is metadata:** the Relay sees when the Node is up. Settings › Node says so where it's turned on. Without a Relay the notice isn't available, and a device that reaches the Node directly sees its status anyway.

### 4a. Amendment proposed (2026-11-26, owner approval needed): how devices reach the Node by default
> **Status of this section: proposed.** It follows the owner's direction of 2026-11-26. Once approved, it replaces §4's "How a client reaches the Node is the user's choice" with the defaults below. Everything else in §4 stays.

**Why:** the people who need a Node most are the least likely to set up port forwarding or a VPN. An always-on box with an open port is the most common way such boxes get attacked. So the default must reach the Node from anywhere without opening anything.

#### The defaults
1. **On the same network: direct.**
   - The Node listens only on local-network addresses: private IPv4 ranges (RFC 1918), IPv6 unique-local (`fc00::/7`) and link-local addresses.
   - Devices find it by mDNS, which can be turned off, or by its address. They connect over the paired TLS channel with pinned keys (§2).
   - **The Node never asks the router to open a port.** There's no UPnP and no NAT-PMP.
2. **Elsewhere: through the Relay.**
   - Devices and the Node exchange sealed messages through their restricted mailboxes (ADR-0016 §4.2–4.3), holding a long-poll or WebSocket open while they're online (ADR-0016 §2), so delivery is close to live.
   - This needs a Relay the user can use: the hosted one or their own (ADR-0016 §7).
   - **Without a Relay,** devices away from the Node's network wait until they're back on it, and Settings › Node says so.
3. **An opened path only if the user chooses it,** in Settings › Node.
   - **Choices:** listen on another interface (a VPN or overlay such as WireGuard or Tailscale), a public interface, a forwarded port, or a reverse proxy that passes TLS through. A proxy that ends TLS still breaks pairing on purpose (§4).
   - **Each choice names what it exposes,** for example "Anyone on the internet can reach this port. Only paired devices can connect, but the Node will see connection attempts". It can be undone in one step.

**Which route a device uses:** it tries the direct route first (the address it knows, then mDNS), then the Relay. The device list and the status screen show each device's current route ("direct", "through the Relay", "not reachable").

#### What works on each route

| | Same network or opened path | Through the Relay |
|---|---|---|
| Sync of changes (dn-sync §10) | Yes | Yes (sealed envelopes, chunked to the Relay's 1 MiB message limit) |
| Held approvals, answers and notifications (§6) | Yes | Yes (ADR-0016 §4.2) |
| Add-on views (ADR-0013 §9.3, if approved) | Yes | Yes (a view document fits in one message) |
| Offline notice (§4) | Status seen directly | Yes |
| **Snapshot** for a new or long-absent device (dn-sync §9) | Yes | **No.** It's the whole data set and stays off the Relay (dn-sync §9) |
| **The desktop's model used by the Node** (§5) | Yes | **No.** Streaming a model through store-and-forward mailboxes is too slow. The job fails clearly: "Your desktop isn't reachable directly, and this job uses its model" |
| Pairing | Yes (§2) | Yes, for a Node with no shared network (below) |

- **Consequence for full devices:**
  - A desktop that has been away for longer than the Relay's TTL (ADR-0016 §6, 7 days by default) needs a snapshot, and waits for the direct route. The device list says "Needs to be on the Node's network once to catch up".
  - Devices that sync only what they show, such as a phone app for approvals (its own ADR), don't need snapshots.
- **Pairing through the Relay,** for a Node that shares no network with the device (a VPS):
  - The pairing code (§2) also carries an **open, one-message pairing mailbox** on the Node's Relay, like an invitation (ADR-0016 §4.4).
  - The new device seals its join request (its public keys and the proof of the one-time secret) to the Node's key and delivers it there.
  - Both sides show the check phrase, and the user confirms as in §2.
  - There's no TLS channel to bind the proof to, so the proof is bound to the Node's key, the mailbox id and the one-time secret. The mailbox takes one message and closes, so a proof can't be replayed.
  - The pairing code expires after 10 minutes as before.

#### Where the Node runs
- **Home server, NAS, Mac mini or PC:** devices at home connect directly, and the Relay covers everything else. No port is opened.
- **VPS:** it shares no network with the user's devices, so everything goes through the Relay unless the user opens a path (default 3). Pairing uses the Relay (above).
- **Docker or Podman** (§9): the image and the example compose file publish **no port**. Direct access on the home network needs either host networking or publishing the port on a local address only, and the install docs show both.

#### Checked against §4 and ADR-0016
- **"The Node never needs Demios servers" stays true.**
  - On the same network, nothing else is involved.
  - Away from it, the user can run their own Relay (ADR-0016 §7).
  - With the hosted Relay, a Demios server carries ciphertext only, and sees what ADR-0016 §3 lists.
  - What changes: **the hosted or own Relay becomes the default away-from-home path.** The pairing screen and Settings › Node say so, and say what the Relay sees: mailbox ids, timing and sizes for all remote traffic.
- **ADR-0016 needs no change.** Restricted mailboxes with delivery grants, the size limits, long-poll and WebSocket, and the open one-message mailbox are all there already. Pairing through the Relay is a new use of the invitation pattern (§4.4), and is listed here rather than there.
- **Push (§4) is unchanged:** content-free and opt-in per device.

#### Options considered
| Option | Away from home | Exposure | Setup | Notes |
|---|---|---|---|---|
| A. Direct only; the user sets up a VPN or port (today's §4) | Only with the user's setup | Depends on the user | High | Most users never get past it |
| **B. Same network direct, Relay elsewhere, a port only by choice (chosen)** | Yes, with a Relay | None by default | None beyond pairing | Snapshots and the desktop's model need the direct route |
| C. Always through the Relay, even at home | Yes | None | None | Slower at home for no gain, and every message's metadata reaches the Relay |
| D. Open a port automatically (UPnP or NAT-PMP), or build in an overlay network | Yes | A port, or a network stack in the app | Low | Rejected: an exposed port by default, or a large component to maintain |

#### Action items (to be filed as issues once approved)
1. Node listener: local-network addresses only by default; the opt-in listeners with their warnings; no UPnP.
2. Route selection on devices (direct first, then the Relay), and the route in the device list and status screen.
3. Pairing through the Relay: the one-message pairing mailbox and a proof bound to the mailbox. That updates the pairing primitives (#234) and the design note (`docs/design/dn-sync.md`).
4. Clear failures for the two direct-only features: snapshots, and the desktop's model.
5. Install docs: Docker without published ports, and how to allow home-network access.

### 5. Models on the Node
A Node has three kinds of model, and each chat, job or add-on slot names one **explicitly**:
- **Providers with the user's keys (BYOK).** The keys are on the Node (§7).
- **A local model on the Node**, run by `dn-inference` with the Node's own CPU or GPU. It's downloaded to the Node with the same checks as in the app (licence, fit, SHA-256).
- **The desktop's model, while the desktop is awake.** The Node calls a paired desktop as a remote `ChatModel` over the paired channel. The model and its data stay on the desktop.

**No silent fallback.**
- If the named model is unavailable (the desktop is asleep, a key was revoked, the Node has no GPU memory left), the job **fails clearly** with the reason, for example "Your desktop is asleep. This job uses its model". It's shown in the job's history and as a notification.
- It never switches to another model or provider by itself.
- The user can pick a different model for that job, and that is a visible setting, not an automatic one.

### 6. Approvals while nobody is at the desk
ADR-0013's rules for background jobs apply unchanged on the Node:
- `auto` applies only to tools the user explicitly set to "run without asking";
- every other call is **held**;
- nothing auto-approves, and a held call never times out into a silent "yes" or a silent "no".

The Node adds where the approval happens:
- **Jobs prepare work.** A job can read, search, draft and stage, for example a reply as a draft, or a file change as a proposal. Each risky step becomes a held approval that shows the exact action and its arguments.
- **Held approvals sync to every paired device** (§3). They are shown on whichever device the user picks up, and pushed if the user turned push on (§4).
- **An answer is signed** by the device that gave it. The Node checks the signature against the device group before it runs anything. A forged or replayed answer is refused.
- **An answer applies once**, to that call with those arguments. If the job's next run needs the same action again, it asks again, unless the user chose "run without asking" for that tool, as in the app.
- **Expiry is a refusal, never a run.** The user can set a time after which a held approval is dropped. It's shown as "expired, not run".

### 7. Secrets on a machine without a desktop keychain
A Node needs provider keys, tool tokens, its device key and the key for its database. On a headless server, there's often no unlocked keychain.

**Threat model:**

| Threat | What protects against it |
|---|---|
| Someone with root or administrator rights on the Node, including a VPS provider | **Nothing in this ADR.** They can read memory and every secret. Choosing where to run a Node is choosing whom to trust; the docs say so |
| A stolen disk, or a leaked backup or snapshot | Secrets encrypted at rest with a key that isn't on the same disk in the clear: a TPM, a protected OS store or a service-manager credential |
| A leaked container image or configuration file | Secrets never baked into the image or written into config; they go in at runtime |
| Another, unprivileged user on the same machine | File permissions (owner only) and a dedicated service account |
| Logs | Secrets never logged (ADR-0007) |

**Where secrets are kept**, in this order, using the first one available:
1. **The platform's protected store:**
   - the system keychain on macOS (launchd);
   - DPAPI or Credential Manager under the service account on Windows;
   - systemd credentials sealed to the TPM2 (`systemd-creds`, `LoadCredentialEncrypted=`) on Linux.
2. **An encrypted secrets file** in the Node's data folder, whose key comes from the service manager at start: a systemd credential, a Docker or Podman secret, or a key file on a separate mount.
3. **A passphrase typed at start** (`dn node unlock`). It's the strongest option against a stolen disk, but the Node can't restart unattended. The Node waits locked and says so to its clients.

Rejected or limited choices:
- **Plain environment variables and secrets in config files are refused.** The Node won't start with them.
- **A plaintext secrets file exists only behind an explicit flag** (`--insecure-plaintext-secrets`, for test setups). The Node then shows a warning in every client's view of it. No silent fallback.

**The status screen** in every client shows which store the Node uses.

The database is encrypted at rest with SQLCipher (ADR-0007's option), with its key in the same store. Full-disk encryption is still recommended, and the install docs say so.

### 8. Updates, logs and "Delete all" across devices
- **Updates:**
  - The Node never updates itself silently.
  - Clients show "Node is on 1.4.0; 1.5.0 is available".
  - It updates the way it was installed: a new image tag, or the package manager, or `dn node update` after the user confirms. Updates are signed with the same key and checks as the app's.
  - **Version skew:** client and Node exchange a protocol version when they connect. A different major version refuses to sync, with a clear message naming which side to update. There's no partial sync.
- **Logs:**
  - Logs stay on the Node, rotated and size-capped.
  - They hold events and errors, never message content, prompts, tool arguments or secrets.
  - A paired device can read them. A debug level that includes content is opt-in, time-boxed, and labelled while it's on.
  - No telemetry (ADR-0009).
- **Delete all local data** on one device deletes that device's data, as today. The confirmation adds two separate choices:
  - **"Also delete everything on the Node and on every paired device."** It sends a signed wipe command. The Node runs it at once: it pauses jobs, deletes its data and secrets, and unpairs every device. Paired devices run it when they next connect to the Node. The report lists devices that haven't confirmed yet.
  - **"Unpair this device"** only, for a device being handed on, so it stops syncing without wiping the others.
  - Before deleting keys, the wipe revokes shared links (ADR-0015) and closes the user's Relay mailboxes (ADR-0016), because the keys that manage them go with it.
  - What a wipe can't reach (data already sent to a provider or a tool server, a link or mailbox it couldn't reach) is listed, as it is in the app.
- **Uninstalling** the Node doesn't delete its data folder by itself. `dn node delete-all` does, with the same confirmation.

### 9. Install paths
Each path runs the Node as a **dedicated, unprivileged user**, with its data in one folder:

| Path | For | Notes |
|---|---|---|
| **Docker or Podman image** | NAS, VPS, home server | Multi-arch (amd64, arm64). Runs as non-root with a read-only root file system and one volume for data. Secrets come from Docker secrets. The image is signed, and its digest is listed in the release notes |
| **systemd unit** | Linux server or PC | A unit file in the `.deb` and `.rpm`. Hardening: `ProtectSystem=strict`, `NoNewPrivileges`, `PrivateTmp`, `StateDirectory`. Secrets come from `LoadCredentialEncrypted=` |
| **launchd** | Mac mini or other always-on Mac | A LaunchDaemon (runs with no one logged in; system keychain), or a LaunchAgent (runs while the user is logged in; their keychain). Installed by `dn node install`, which asks first |
| **Windows service** | Windows PC or server | Registered by `dn node install` under its own service account. Secrets in DPAPI for that account |

- **GPU access** for a local model is documented per path. That covers the NVIDIA container runtime on Docker; on macOS, a Metal-capable LaunchAgent session.
- The desktop app never installs a Node behind the user's back. Installing one is a separate, explicit step, so ADR-0013's "the app installs no daemon" stays true for the app itself.

## Options considered

### Where always-on work runs

| Option | Always on | Privacy | Effort | Notes |
|---|---|---|---|---|
| **A. Headless Node on the user's hardware (chosen)** | Yes, while their machine is on | Data on hardware they control; decrypted there | Medium | Same engine; no Demios servers needed |
| B. Keep the desktop app running (no sleep, login item) | Only while the computer is awake and logged in | Same as today | Low | Doesn't cover a phone, or a laptop that closes |
| C. Demios-hosted Node | Yes | Demios would hold decrypted data | High (operations, security, compliance) | Deferred by the owner |
| D. Run jobs on the phone | No (OS background limits) | On the phone | High | Not reliable on iOS or Android |

### Pairing

| Option | Security | Usability | Notes |
|---|---|---|---|
| **QR code or link with the Node's key and a one-time secret, plus a check phrase (chosen)** | Strong: the key is pinned from the start | Good with a camera; copy and paste without one | No PAKE dependency |
| Short typed code with a PAKE (SPAKE2 or CPace) | Strong if the PAKE is correct | Best for typing | Adds a less common crypto dependency |
| Trust on first use | Weak against an attacker on the network during pairing | Easiest | Rejected |
| Account with a password | Needs a server | Familiar | Rejected: no account |

### Transport

| Option | Notes |
|---|---|
| **TLS 1.3 (rustls) with pinned self-signed keys (chosen)** | `rustls` is already in the tree; HTTP and WebSocket tooling work as usual; a phone can use the platform TLS stack with pinning |
| Noise (`snow`) | Simpler handshake and no certificates, but a new dependency, and the phone needs a Noise library |
| WireGuard built in | Strong, but a whole network stack inside the app |

### Secrets on a server

| Option | Unattended restart | Stolen disk | Notes |
|---|---|---|---|
| **Platform store or TPM (chosen first)** | Yes | Protected | Not available everywhere (Docker, older NAS) |
| **Encrypted file, key from the service manager (chosen second)** | Yes | Protected only if the key isn't on the same disk | Docker secrets live on the host; the docs say so |
| **Passphrase at start (chosen third)** | No | Protected | For users who prefer that |
| Plain environment variables | Yes | Not protected | Refused |
| Plaintext file | Yes | Not protected | Only behind an explicit test flag, with a warning |

## Trade-offs
- **The Node holds decrypted data.** That's the cost of always-on work. A machine at home keeps it in the user's hands. A VPS puts it in the hands of the hosting provider too. The docs and the pairing screen say so, in plain words.
- **A live connection to the Node** from outside the home network needs the user's own VPN or forwarded port. That's harder to set up, and it keeps Demios out of the data path. The optional Relay (ADR-0016) covers the case where devices aren't online together, with ciphertext only, and the user can run their own.
- **No phone push without a push service** is a platform limit, not a choice. Push is opt-in, content-free, and approvals still work without it, just later.
- **Held approvals make unattended work slower.** That's deliberate: an approval needs a person.
- **Full copies on every runtime** cost disk space, and widen where data lives. They also make the app work offline, with or without the Node. Removing a device can't wipe a device that's offline or stolen, and the confirmation says so.

## Consequences
- **`dn`** gains a `node` command group: `run`, `pair`, `install`, `unlock`, `update`, `delete-all`.
- **`dn-sync`** stops being a placeholder. It gets the device group, the change log, tombstones and the paired channel, specified first in a design note.
- **`dn-secrets`** gains server stores: TPM-sealed systemd credentials, DPAPI under a service account, an encrypted file with an external key, and a passphrase unlock. It also reports which one is in use.
- **`dn-core`** gains a device identity, and a signed approval answer on held approvals. That's a contract change, and this ADR is its reference.
- **The app** gains Settings › Node: pair, devices, the Node's status (version, secret store, how it's reached, push), logs, and the two "Delete all" choices.
- **Dependencies** are added only with the lead's approval and recorded in `docs/dependencies.md`. Likely candidates are a QR encoder, mDNS, and an Ed25519/X25519 implementation if the existing tree doesn't already provide one.

## Action items (to be filed as issues)
1. `dn-sync` design note: records, clock, tombstones, conflicts, group key and rotation.
2. Device identity and pairing: keys, QR code and link, check phrase, device list, removal.
3. Paired transport: rustls with pinned keys, protocol version handshake.
4. `dn node run`: the headless runtime with the scheduler and held-approvals queue from ADR-0013.
5. Server secret stores in `dn-secrets`, with the status shown to clients.
6. Remote `ChatModel` to a paired desktop, and clear failures when it's asleep.
7. Signed approval answers, and held approvals synced to every device.
8. "Delete all" across devices: the signed wipe, unpairing, and the report.
9. Install paths: Docker image, systemd unit, launchd, Windows service, and their docs.
10. Threat model for the Node (extends ADR-0009): the trust boundary of the Node's host, pairing, transport, sync and push.
11. Optional push: a content-free wake-up through the Relay (ADR-0016 §4.2) or the user's own push server.
12. Optional offline notice: the Node's sealed "alive" record in the user's own Relay mailbox, and the "offline since" message on paired devices.
