# dn-sync: how devices and the Node keep one data set

**Status:** Draft for review · **Date:** 2026-11-26 · **Implements:** [ADR-0014](../adr/0014-demios-node.md) §2, §3, §6 and §8 (action 1), with [ADR-0016](../adr/0016-demios-relay.md) §4.3 · **Crate:** `dn-sync`

This note specifies sync before any sync code is written. It covers:
- what a record is;
- the clock;
- deletes and tombstones;
- conflicts;
- the group key and its rotation;
- how Relay mailboxes carry changes when devices aren't online together.

Pairing, the paired transport, the Node runtime and the server secret stores each get their own PR (ADR-0014 actions 2 to 5). This note only states what sync needs from them.

## 1. Goals and limits

**Goals**
- Every runtime (the desktop app, the Node, later a phone) keeps a **full local copy** and works offline. Sync never makes a local action wait for the network.
- **Every runtime ends with the same state** once it has seen the same changes, whatever order they arrived in.
- **Nothing is lost silently.** Concurrent text edits keep both versions (§6). Changes that can't be applied yet are listed with the reason (§9).
- **Nothing between the devices can read or change a record:** not the network, not a Relay, not a backup of the sync log (§7).
- **A removed device can't send or read anything new** (§8).

**Limits** (stated, not solved here)
- The Node and every paired device hold the data decrypted (ADR-0014 "Be honest"). Sync protects data between devices, not on them.
- A **paired device that is compromised** can do anything the user can do: delete chats, change settings, approve calls. Sync limits what it can do in secret: every change is signed by the device that made it, so the change history names it, and a device that sends two different changes under one number is caught (§9.3). Removing the device ends it (§8).
- **Metadata on the Relay** (mailbox ids, sizes, timing) is as ADR-0016 §3 states.

## 2. Terms

| Term | Meaning |
|---|---|
| **Device** | One runtime with its own key pairs: a desktop app install, a Node, a phone. |
| **Device group** | The devices paired with one Node. A group has exactly one Node (§8.1). |
| **Record** | One synced item, for example a chat, a message, a project, a setting. It has a collection, an id and fields. |
| **Change** | One local transaction's edits to records, made by one device. |
| **Envelope** | A change, signed by its author and sealed with the group key. It is the only form a change takes outside the device that applies it. |
| **Log** | The envelopes a device keeps to pass on to other devices. |
| **Vector** | For each author, the highest change number a device has applied with no gaps. |
| **Epoch** | A numbered group key. It changes when a device is removed. |

## 3. Records

### 3.1 Identity
- A record is identified by **(collection, id)**. Ids are UUIDv7, as every synced table already uses (`dn_core::Id`). Settings use their key as the id.
- Collection names are fixed strings in code, for example `chat`, `message`, `project`, `project_file`, `artifact`, `artifact_version`, `chat_skill`, `addon_memory`, `setting`, `approval`. A new collection is a code change with its own tests.

### 3.2 Which data syncs (ADR-0014 §3)

| Collection | Source today | Kind (§3.3) | Notes |
|---|---|---|---|
| `chat` | `store_conversations` | fields | Title is text; model, project and the trash time are scalars |
| `message` | `store_messages` | append | Synced once it leaves `streaming`; never while it is still streaming |
| `project` | `store_projects` | fields | Name and instructions are text |
| `project_file` | `store_project_files` + the file | fields + blob | The content goes as a blob addressed by SHA-256 (§10.3). `source_path` is per device and doesn't sync |
| `artifact`, `artifact_version` | `artifact_*` | fields, append | Needs a version id; see §6.4 |
| `chat_skill` | `app_conversation_skills` | fields | |
| `addon_memory` | add-on memory (ADR-0013 §5) | fields | Item text is text |
| `setting` | `app_settings` | fields | Only keys declared group-wide (§3.4) |
| `approval` | held-approvals queue (ADR-0013) | state machine | §6.5 |

**Never synced:** downloaded models, the knowledge index (each runtime rebuilds it), secrets (unless the user asks per key, in a later change), logs, the tool side-effect ledger, knowledge sources (they are folder paths on one machine), and approvals to launch local tool servers (they name a command line on one machine).

### 3.3 Three kinds of record
- **Fields:** the record is a set of named fields, and each field is merged on its own (§6). Most records are this kind.
- **Append:** the record is written once and never changed, for example a finished message or an artifact version. Two devices can't edit the same one, so there is nothing to merge. A delete is still possible (§5).
- **State machine:** the record moves through fixed states, and only one device decides the outcome (§6.5).

### 3.4 Settings
Each setting key declares its scope in code: **group** (syncs) or **device** (stays local, for example window size or the loaded local model). There is no default: a test fails if a key has no declared scope, so a new setting can't start syncing, or stop syncing, by accident.

### 3.5 Who owns the tables
Each crate keeps its own tables (CLAUDE.md). `dn-sync` never reads or writes another crate's tables. Each owning crate provides an **adapter** for its collections, through a trait in `dn-sync`:
- `changed_since(rev)`: the records written since a local revision number, with their field values;
- `deleted_since(rev)`: the records deleted or purged since then;
- `apply(record)`, `trash(id)`, `purge(id)`: write a merged result.

The owning crate adds a `rev` column to each synced table, bumped by its own trigger from its own counter, in its own migration. The app and the Node register the adapters when they open the store.

### 3.6 How a local edit becomes a change
`dn-sync` doesn't need every write to call it. It finds local edits by asking the adapters what changed since the last scan, then comparing each field with the value it last recorded (a SHA-256 per field, in `sync_fields`). The fields that differ become one change.

**One rule makes this exact: scan, then apply.** Before `dn-sync` applies any incoming change, it first scans for local edits and turns them into a change, in the same write transaction. A local edit is therefore always recorded as made **before** the incoming change it never saw, so it can never silently overwrite that change (§6.2). Writes made through `apply` update the recorded hashes, so they are never sent back as local edits.

## 4. The clock

### 4.1 Change numbers and vectors
- Every device numbers its own changes **1, 2, 3, …** with no gaps (`seq`).
- Every change carries its author's **vector** at the time it was made (`deps`): for each device, the highest change from it that the author had applied.
- A device **applies a change only after everything in its `deps`** (causal order). A change that arrives early waits in the inbox (§9.1).

This is what tells "later" from "at the same time": change B comes after change A if B's `deps` include A. If neither includes the other, they were made at the same time (concurrent). Wall clocks play no part in that decision.

### 4.2 Hybrid logical clock
Each change also carries a **hybrid logical clock** (HLC) time, used:
- to break ties between concurrent writes to the same scalar field (§6.1);
- to order items from different devices for display, for example two messages written at the same time on two devices.

Its format is 64 bits: 48 bits of Unix milliseconds and a 16-bit counter. Ties are broken by the author's device id, so every device makes the same choice.
- **Sending:** `hlc = max(last_hlc + 1, now_ms << 16)`.
- **Receiving:** `last_hlc = max(last_hlc, incoming.hlc)`.

### 4.3 Clocks that are wrong
A device whose clock is far ahead would win every tie and push everyone's clock forward with it. So:
- An incoming change whose HLC time is **more than 10 minutes ahead** of the receiver's clock is **not applied**. It is kept in the inbox, and Settings › Node says "*Work phone*: its clock is 2 hours ahead. Fix its date and time to sync it."
- When the receiver's own clock catches up, the held change applies, and the changes after it follow. The receiver never moves its own clock to match.
- A clock that's behind does no harm. That device just loses ties.

## 5. Deletes and tombstones
There are two kinds of delete, as in the app today:
- **Move to trash** sets the record's trash time. That's an ordinary scalar field: it syncs, and restoring is a later write (§6.1).
- **Delete for good (purge)** writes a **tombstone**: the collection, the id, the time and the author. Every runtime then deletes the record's content and the data that depends on it (a chat's messages, artifacts and blobs no other record uses), as the app's purge does today.

**Rules:**
- **A tombstone wins.** A write to a purged record that arrives later, or was made at the same time, is dropped. Children that arrive for a purged parent are dropped too, for example a message for a deleted chat. When that drops an edit a device made at the same time, that device shows a notice saying what was dropped and where the delete came from, for example "*Weekly plan* was deleted on *Node* while this device was changing it." That notice is the one place sync reports a write it dropped, and it never drops one without it.
- **Tombstones are kept for good.** They hold no content (an id, a time and an author, about 50 bytes), and keeping them stops a device that comes back after months from bringing a purged record back.
- **Where the content goes:** it leaves every device's local state when the tombstone is applied. It leaves the sync logs once every device has acknowledged the changes that carried it (§9.2). On a Relay it is deleted when it's picked up, or at the end of its TTL (ADR-0016 §6). The "Delete for good" confirmation says this in one line.

## 6. Conflicts
A conflict needs two writes to the same field that were **concurrent** (§4.1). A write that comes after another simply replaces it.

### 6.1 Scalar fields: last write wins
For booleans, numbers, ids, model references, trash times and setting values, the write with the higher HLC wins (ties broken by device id). That's ADR-0014's "a field's last write wins". The losing value isn't kept: these are values the user picks from a list, so picking again is the fix.

### 6.2 Text fields: both versions are kept
Fields marked as text are chat titles, project names and instructions, add-on memory text, and text setting values:
- The higher HLC is shown as the current value. **The other value is kept** in `sync_conflicts`, with its author and time.
- The UI marks the field: "Changed on two devices. Keep this, keep the other, or edit." It shows both, labelled by device.
- Choosing is a normal write whose `deps` include both versions, so it ends the conflict on every device.
- More than two concurrent versions are all kept.
- **Sync never merges text** automatically. A character-level merge (a CRDT) is a possible later change for long text; it isn't needed for the fields here.

### 6.3 Messages
Messages are append records, so two devices that add to one chat at the same time both keep their messages. They are shown in order of (HLC, id). Both devices then show the same order, even if it interleaves two conversations. The chat shows a one-line notice where that happens: "Messages from two devices at the same time."

### 6.4 Artifact versions
Today a version is numbered `n` inside its artifact, so two devices that each add a version would both create the same `n`. Before artifacts sync, `dn-artifacts` needs a migration:
- each version gets its own UUIDv7 id and names its parent version by id;
- `n` becomes a display number, worked out from (HLC, id).

Two versions with the same parent are then a **fork**. The artifact panel shows "Edited on two devices", both versions are kept, and the user picks which one carries on. That migration belongs to the crate's owner and is listed in §12.

### 6.5 Approvals
A held approval (ADR-0013, ADR-0014 §6) is a state machine: `held` → `answered` → `ran` or `failed`, or `held` → `dismissed`, or `held` → `expired`.
- **Any device can write an answer.** The answer is signed by the device that gave it (ADR-0014 action 7) and names the call and a SHA-256 of its exact arguments.
- **Only the runtime that runs the job writes the outcome.** That's the Node when one is paired (ADR-0013 §3: one scheduler at a time). It checks the answer's signature against the group's current members, checks that the arguments' hash matches, and uses the first valid answer it receives.
- **A second answer is never applied.** If the phone approves and the desktop denies at the same time, the outcome names the answer that was used and the device that gave it, and the other device shows "Already answered on *Work phone*: approved."
- **An answer applies once,** to that call. A replayed answer names a call that's no longer `held`, so it does nothing.
- **Expiry is a refusal** (ADR-0014 §6). The Node writes `expired, not run`.

## 7. Envelopes and encryption

### 7.1 What a change looks like
```text
Change {
  v: 1,                    // format version
  group: GroupId,          // 16 random bytes
  epoch: u32,              // group key it is sealed with
  author: DeviceId,
  seq: u64,
  deps: { DeviceId: u64 },
  hlc: u64,
  ops: [ Put { collection, id, fields: { name: value } }
       | Append { collection, id, value }
       | Purge { collection, id } ]
}
```
- It's serialised as JSON (serde_json, already a dependency).
- **The signature covers the exact bytes,** which are never re-serialised. So no canonical encoding is needed.
- Field values are JSON. Blobs travel separately (§10.3).

### 7.2 Sign, then seal
1. The author signs the change bytes with its **Ed25519** device key.
2. It seals `signature ‖ change` with the group key of the change's epoch:
   - a random 32-byte salt;
   - `HKDF-SHA256(group_key, salt, "dn-sync v1 envelope")` gives a 32-byte key and a 12-byte nonce, used once;
   - **AES-256-GCM**, with `"dn-sync v1" ‖ group ‖ epoch` as associated data.
3. The envelope is `v ‖ group ‖ epoch ‖ salt ‖ ciphertext`. **Outside the seal, only the group and the epoch show.** The author, change number and contents don't.

**The receiver** opens the seal, then checks the signature against the author's key **as the group's membership stood at that epoch** (§8), then checks `group`, `epoch` and `seq`. Any failure refuses the envelope, and the device list counts refusals per sender.

**Why both, over a TLS link that's already private:**
- The same sealed bytes go to a peer, to a Relay mailbox or into the log, so one format covers every route.
- Any device can pass on another device's envelope without being able to forge it.

### 7.3 Crypto and dependencies
- **`aws-lc-rs` is already built into the app** for rustls. It provides Ed25519, X25519, AES-256-GCM, HKDF-SHA256, SHA-256 and a system random number generator. Using it directly adds no new code to the build, but it becomes a **direct dependency**. That needs the lead's approval and a `docs/dependencies.md` row, in the first PR that uses it.
- **Key wrapping to one device** (§8.3) uses **HPKE** (RFC 9180, base mode: X25519, HKDF-SHA256, AES-256-GCM), the suite ADR-0016 already names. `aws-lc-rs` has no HPKE API, and sync won't hand-roll one. The candidate is the `hpke` crate, to be approved with the Relay client, which needs it anyway.
- No other crypto. No new crypto designs: everything above is a standard building block.

## 8. The group, its key and rotation

### 8.1 One Node per group
A group always has exactly one Node. **The Node orders membership changes.** A desktop app with no Node paired syncs nothing, which matches ADR-0014: the app works fully without a Node. A group without a Node (for example a desktop and a phone only) would need a different way to order removals. It's left out of this note (§13).

### 8.2 Membership
The membership is a **hash-chained list of signed events**. Each event names the hash of the one before it, and the Node signs every event:

| Event | Holds |
|---|---|
| `Create` | the group id, the Node's public keys, epoch 0 |
| `Add` | the device's Ed25519 and X25519 public keys, its name and kind, and which device approved the pairing |
| `Remove` | the device, who asked for the removal, and the **last change from it that is accepted** (its `seq` in the Node's vector at that moment) |
| `Rotate` | the new epoch, and the new group key wrapped to each remaining device (§8.3) |
| `Rename` | a device's display name |

- **Every device keeps the whole chain** and checks each event's signature and its link to the event before. A fork in the chain, meaning two events naming the same previous event, can only come from a compromised Node key. The device stops syncing and says so.
- **A removal can be asked for from any paired device** (ADR-0014 §2). The request is signed and goes to the Node, directly or through the Node's mailbox. Once the Node has written `Remove`, **every device refuses that device's changes after its last accepted `seq`, and the Node refuses its connections.** The device that asked **holds** the removed device's new changes in its inbox at once, without waiting for the Node. Once `Remove` arrives, it applies those up to the last accepted `seq` and drops the rest. It never refuses on its own, because another device might accept the same changes, and the two devices would then differ for good.
- Changes from the removed device up to its last accepted `seq` stay, so nothing already synced is undone.

### 8.3 The group key
- **It's created by the Node** at `Create`: 32 random bytes. It's kept in each device's secret store (`dn-secrets`), never in SQLite and never in logs.
- **A new device gets the current key** in the pairing exchange, wrapped to its X25519 key with HPKE, once the check phrase is confirmed (ADR-0014 §2).
- **Rotation happens on every removal,** and when the user asks ("Rotate keys" in Settings › Node):
  1. The Node makes a new key for epoch `e + 1`.
  2. It wraps that key to each remaining device and writes `Rotate`.
  3. Devices seal new changes with `e + 1` as soon as they've applied `Rotate`, and never seal with `e` again.
  4. Envelopes sealed with `e` are still accepted from authors who are still members (they may have been in flight), but never from the removed device beyond its last accepted `seq`.
- **Old keys** are kept until the log holds no envelope sealed with them (§9.2), then deleted from the secret store.
- **The cost is a short window:** a change sealed with `e` by a device that hadn't seen `Rotate` yet could be read by the removed device, but only if that device can still fetch it. It can't, because the Node refuses it and the mailboxes are rotated (§10.2). The threat model (§11) lists this.

### 8.4 If the secret store can't be read
Without the group key, sync can't run. The device shows the same keychain message as Settings › Models (#200) and stops syncing. It never falls back to a key kept anywhere else.

## 9. Applying, acknowledging and compacting

### 9.1 The inbox
An incoming envelope is checked (§7.2) and stored in `sync_inbox` before it's acknowledged to a Relay or a peer, so a crash loses nothing. It's applied once its `deps` are met (§4.1). The device list shows what's waiting, for example "Waiting for 3 changes from *Node*." A change that waits because of a wrong clock (§4.3) or a removal (§8.2) says why.

### 9.2 Acknowledging and compacting the log
- Each device publishes its **vector** as a small scalar record (`sync_vector`, one per device), so every device knows what the others have applied.
- An envelope can leave the log once **every current member** has applied it. The log then keeps only `(author, seq, SHA-256 of the envelope)`, so later duplicates are still recognised.
- **A device that hasn't been seen for 90 days** stops holding the log back. The device list says "*Old laptop* hasn't synced for 90 days. It will need a full copy when it's back." When it comes back:
  1. it gets a **snapshot** (§9.4);
  2. it sends its own changes that nobody has yet;
  3. those merge by the usual rules.

### 9.3 Replays and duplicates
- A change is identified by **(author, seq)**. If it was already applied, a second copy is ignored. That makes every route safe to repeat, and an old envelope sent again does nothing.
- The signed bytes include the group, the epoch and the change number, so an envelope can't be replayed into another group or under another number.
- **Two different envelopes with the same (author, seq)** mean the author's key was used to sign two histories (equivocation). A device that already applied one copy keeps it. The other copy is refused everywhere, and the conflicting pair is passed on to the other devices as evidence. Which copy wins is decided by the Node: it names the copy in its vector, and devices that applied the other one undo it from the snapshot (§9.4). That device is marked "sent conflicting changes" in the device list, its later changes are held, and the user is offered **Remove device**.

### 9.4 Snapshots
- A new device, or one past the 90-day line, starts from a **snapshot**: the current state of every synced record, the tombstones, the membership chain and the sender's vector.
- It's sent only over the paired channel (ADR-0014 §2), never through a Relay, because it's large and it's the whole data set.
- A snapshot is signed by the device that sends it, usually the Node. Records in it are accepted on that device's signature. That's the one place a device vouches for others' data. It's limited to devices that are already members, at pairing or on their return.

## 10. Routes: direct, and Relay mailboxes

### 10.1 Direct
When two devices are connected over the paired channel:
1. They exchange their vectors.
2. Each sends the envelopes the other is missing, from its log, including other authors' envelopes.
3. If the other side is behind what has been compacted, it gets a snapshot instead (§9.4).

While they stay connected, new changes go straight away. The desktop app keeps its connection to the Node open while it runs (ADR-0014 §4).

### 10.2 Relay mailboxes (ADR-0016 §4.3)
- **Each device has one restricted sync mailbox** on the Relay the user chose, or none. Its id and the Relay's address are part of the device's `Add` event, so every member knows where to deliver.
- **Each device grants delivery** to every other member's signing key (ADR-0016 §1). Grants are short-lived (30 days) and renewed as devices sync.
- **When to deliver:** after a change, a device delivers its envelope to the mailbox of **every member it isn't connected to directly**. A group has a handful of devices, so the cost is small. Duplicates are expected and ignored (§9.3).
- **Fetching:** a device fetches its mailbox when it starts, when it reconnects to the network, and over a long-poll while it runs. It acknowledges each message once it's in the inbox (§9.1), and the Relay deletes it then.
- **On removal,** every remaining device **rotates its mailbox:** it closes the old one, makes a new one and issues new grants to the remaining members. The removed device's grants die with the old mailbox, so it can't fill the new ones. The new mailbox ids go to members in a signed record.
- **Size:**
  - Envelopes are padded to the buckets 1, 4, 16, 64 and 256 KiB, and 1 MiB (ADR-0016 §3).
  - A change bigger than 1 MiB, after padding, is split into chunks (§10.3).
  - The mailbox's caps (ADR-0016 §5) are shown to the owner, and a full mailbox is reported, never ignored.
- **TTL:** a device that was offline longer than the mailbox's TTL misses messages. It notices the gap in `seq` (§9.1) and catches up directly from the Node. **The Relay is never the only copy** (ADR-0016 §4.3).
- **Without a Relay,** devices sync only when they reach each other directly, and Settings › Node says so (ADR-0014 §4).

### 10.3 Blobs
- Project files and other large content travel as **blobs**, addressed by SHA-256 and sealed like envelopes, in chunks of at most 1 MiB.
- A record names its blob by hash, and the receiver checks the hash before it uses the blob.
- Through a Relay, blobs go up to **64 MiB**. Bigger ones wait for a direct connection, and the file shows "Waiting to sync directly."
- A blob no record uses any more is deleted, like the app's purge.

## 11. Threat model notes
This extends ADR-0014 action 10. A full Node threat model is its own document, `docs/security/node.md`.

| Threat | What protects against it | Test |
|---|---|---|
| Someone on the network, or a proxy, reads or changes sync traffic | Paired TLS with pinned keys (ADR-0014 §2), plus sealed and signed envelopes | Tampered envelope refused; envelope from a non-member refused |
| The Relay operator reads changes | Sealed with the group key; the author and contents are inside the seal | Envelope bytes hold no plaintext (checked in tests on known plaintext) |
| An old envelope is sent again (replay) | (author, seq) dedupe; the group and the number are signed | Replay is a no-op; replay into another group refused |
| A removed (stolen) device keeps sending | `Remove` names its last accepted `seq`; the Node refuses its connections; mailboxes rotate | Its later changes refused, through any route |
| A removed device reads new changes | Group key rotated; it can't fetch from the new mailboxes or the Node | New epoch can't be opened with the old key |
| A member signs two histories | Equivocation detected; the Node's copy wins everywhere (§9.3) | Second copy refused, devices converge on the Node's, and the device is flagged |
| The device that asked for a removal and the others disagree | The asking device holds, never refuses, until `Remove` arrives (§8.2) | Removal asked offline: every device ends with the same changes |
| A device's clock is far off | The 10-minute limit; the change is held, and the device is named (§4.3) | Change from the future held, then applied once the clock allows |
| A purge leaves content behind | Tombstones win; the log is compacted once all have applied; Relay TTL | Content gone from state and logs after acknowledgements |
| Sync drops an edit without saying so | Text conflicts keep both versions; dropped writes to purged records show a notice | Concurrent text edit keeps both; notice on a dropped edit |
| Forged or replayed approval answer | Signed answer, checked against members; arguments hash; applies once (§6.5) | Forged answer refused; second answer not applied |
| The Node's key is stolen | Out of scope, like root on the Node (ADR-0014 §7). A fork in the membership chain is detected | Forked chain stops sync |

## 12. Changes needed in other crates (each by its owner, with tests)
- **`dn-core`:** a device identity type and a signed approval answer. ADR-0014 is the reference (its "Consequences" section names both).
- **`dn-store`, `dn-artifacts`, the app's settings and skills tables:**
  - a `rev` column and trigger on each synced table;
  - an adapter (§3.5);
  - for `dn-artifacts`, version ids and parents (§6.4).
- **`dn-secrets`:** ids for the group key per epoch; the server stores come from ADR-0014 action 5.
- **The app:**
  - Settings › Node, with the device list, its waiting and refused counts, and the clock and conflict messages;
  - the conflict UI for text fields and forked artifacts.

  These are designed in the design system first, for the owner's review.

## 13. Open questions for review
1. **A group with no Node** (a desktop and a phone only). This note requires a Node, because it orders removals. Is that fine for now?
2. **The 90-day line** for a device that's been away (§9.2), and the **10-minute clock limit** (§4.3). Are these the right numbers?
3. **The 64 MiB limit** for blobs through a Relay (§10.3), against the 256 MiB restricted mailbox cap.
4. **Message order across devices** (§6.3): shown by HLC with a notice. The alternative is showing them as two branches.

## 14. Implementation order for `dn-sync`
Each step is its own PR with tests, after pairing and the paired transport land:
1. **HLC, vectors, change format, sign and seal, the inbox.**
   - Tests: convergence of three replicas over many random orderings (seeded, no new test dependency);
   - replay; tampering; a change from the future.
2. **Membership chain, removal and rotation.**
   - Tests: a revoked device's changes are refused through every route;
   - a forked chain; old-epoch envelopes in flight.
3. **Adapters, scan-then-apply, conflicts and tombstones** for chats, messages and projects, then artifacts once §6.4 lands.
4. **Direct route and snapshots.**
5. **Relay route:** mailboxes, grants, padding, chunking, rotation on removal, catch-up after TTL.
6. **Blobs.**
7. **Approvals** (with ADR-0014 action 7).
