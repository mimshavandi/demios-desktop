# ADR-0016: Demios Relay: encrypted store-and-forward mailboxes, addressed by public key

**Status:** Proposed · **Date:** 2026-11-25 · **Deciders:** project owner, lead · **Extends:** [ADR-0009](0009-security-baseline.md), [ADR-0014](0014-demios-node.md), [ADR-0015](0015-encrypted-sharing-and-forms.md) · **Issue:** #207

## Context
Several features need to hand something to a device that isn't online right now:
- a form response, while the owner's computer is off (ADR-0015);
- an approval request or a notification for the phone, while the Node waits (ADR-0014 §4, §6);
- sync between the desktop, the phone and the Node when they aren't online at the same time (ADR-0014 §3);
- an invitation to a shared space (a later feature), for someone who isn't online;
- a webhook from a third-party service (mail, calendar, GitHub) for an add-on, while the user's Node is down.

Each of these could get its own small server. That would mean several services to secure, several retention rules and several privacy sections. This ADR makes it **one service, the Demios Relay**: an encrypted, store-and-forward **mailbox** per device or per purpose, addressed by a public key.

**What the Relay is not.**
- It **never decrypts** what it holds.
- It **runs no jobs** and **holds no keys** that could open a message.
- It isn't a hosted Node (ADR-0014 keeps a hosted Node deferred). It can't act for the user, run a model or answer an approval.

The one exception, webhooks, is stated where it applies (§4.5).

**Naming.** Docs and UI say **Relay** and **mailbox**, never "drop box", to avoid confusion with the Dropbox product.

This ADR decides the design. It doesn't implement it.

## Decision

### 1. Mailboxes
- **A mailbox is addressed by a public key.** Its id is a hash of an X25519 public key whose private key only the recipient holds. Anyone who has the id can know where to deliver. Only the holder of the private key can pick messages up.
- **Mailboxes are per device and per purpose, not per person.** A device has one for sync and approvals. Each form link has its own (ADR-0015). Each webhook source has its own (§4.5). That way the Relay can't link a user's form mailboxes to their devices by id. It may still link them by timing and network address (§3).
- **A mailbox is created by its owner** with a signed request, which proves possession of the key. The request sets the mailbox's limits within the Relay's maximums (§5).
- **Who may deliver** is set per mailbox:
  - **Restricted** (sync, approvals): the sender must present a **delivery grant**, a short statement signed by the mailbox owner that names the sender's public key and an expiry. Paired devices get grants when they pair (ADR-0014 §2).
  - **Open** (form responses, webhooks, invitations): anyone who has the mailbox address can deliver, within the mailbox's limits (§5). The address is shared only where it's needed: inside an encrypted form bundle, in a webhook URL, or in an invitation link.
- **Every message is sealed before it reaches the Relay,** with HPKE base mode (RFC 9180: X25519, HKDF-SHA256, AES-256-GCM) to the mailbox key, or with the device group key for sync (ADR-0014 §3). The Relay stores the ciphertext, a random message id, the size and the time it arrived. **It stores nothing else.**
- **Pick-up and deletion:**
  1. The owner fetches with a signed request.
  2. It decrypts on its own device.
  3. It **acknowledges** the messages, and the Relay **deletes** them at once.
  4. A message that is never picked up is deleted when its **TTL** runs out (§6).
- **Closing a mailbox** (signed) deletes it with everything in it. New deliveries are refused with "This mailbox is closed".

### 2. Transport and API
- HTTPS only (TLS 1.3), with one small JSON API: create, deliver, fetch, acknowledge, close, and for push, register a push token (§4.2).
- Owner requests are signed with the mailbox key (Ed25519 signatures over the request, with a nonce and a time, to stop replays). **No accounts, passwords or bearer tokens.**
- A device that's online can hold a long-poll or WebSocket connection open, so messages arrive at once. That's an optimisation; fetching works without it.

### 3. What the Relay can still see
The Relay can't read messages, but it isn't blind. It sees:

| It sees | Why | What limits it |
|---|---|---|
| **The mailbox id** | It's the address | Ids are per device or purpose, not per person; nothing in the id names the user |
| **Message sizes** | It stores them | Senders pad to size buckets |
| **Timing**: when messages arrive and when they're fetched | It's a mailbox | Nothing; this is inherent |
| **The sender's and the owner's network addresses** | TCP and TLS | Used in memory for rate limits only, as a salted hash with a daily salt; never stored with messages or logged (§6) |
| **That one owner fetches several mailboxes** | Same connection or address | The app can fetch different mailboxes over separate connections; it can't hide them from a determined operator |
| **Push tokens** for phones that turn push on | It sends the wake-up (§4.2) | Stored only while push is on; deleted when it's turned off or the mailbox closes |
| **For webhooks: the payload, briefly in memory** | The third party sends plaintext (§4.5) | Sealed at once; off per source |

The pairing screen, the share sheet and the webhook setup each say which of these applies, in plain words.

### 4. Use cases

#### 4.1 Form responses (ADR-0015)
Each form link has an **open** mailbox. The respondent's browser seals each response to the form's key and delivers it there. The owner's app or Node fetches, decrypts and acknowledges, and the responses are then deleted from the Relay. The form-specific limits (responses per link, field checks, the review panel and the notice) stay in ADR-0015. The Relay enforces its own limits on top (§5).

#### 4.2 Approval requests and notifications to the phone
- The Node delivers each held approval or notification, sealed, to the phone's **restricted** mailbox (ADR-0014 §6).
- **If the phone turned push on,** the Relay sends a **content-free** wake-up through Apple's or Google's push service ("Something is waiting", nothing more). The phone then fetches from its mailbox or connects to the Node directly.
- **This is the push relay** that ADR-0014 §4 describes. There's no separate service. Push needs the app's push credentials, so for push on the official phone app, it's the Demios-hosted Relay or nothing. A self-hosted Relay can't send official app pushes. Instead it can wake the phone through the user's own UnifiedPush or ntfy server on Android.
- The answer to an approval goes back the same way, signed by the phone, to the Node's mailbox, unless the phone reaches the Node directly.

#### 4.3 Sync when devices aren't online together
- Devices sync directly when they can (ADR-0014 §3). When the other side is offline, a device **delivers its changes to the other devices' restricted mailboxes**, already sealed with the device group key.
- Large items (project files, artifacts) are split into chunks within the size limit (§5).
- A device that has been offline longer than the TTL catches up directly from another device or the Node. The Relay is never the only copy of anything.

#### 4.4 Invitations to shared spaces
- An invitation link carries the inviter's **open** invitation mailbox address and a one-time key. The invitee's app delivers its sealed reply (its public key and name) there, and the inviter's device completes the exchange when it next fetches.
- An invitation mailbox accepts **one** message, and closes after that or after its TTL.
- What shared spaces are, and how members are managed, belongs to their own ADR. This section only says they can use the Relay for invitations.

#### 4.5 Webhooks from third-party services, while the Node is down
Some add-ons react to events: a new mail, a calendar change, a GitHub issue. The third party sends a webhook to a URL. If that URL is the user's Node and the Node is down, the event is lost. So the user can give the third party a **Relay webhook URL** instead.

**Here the Relay sees plaintext, briefly.** The third party sends its payload in the clear over TLS, and the Relay is where TLS ends. The Relay:
1. reads the request in memory;
2. **seals it at once** with HPKE to the webhook mailbox's key, which the Relay doesn't hold;
3. stores only the ciphertext;
4. never writes the plaintext to disk or logs.

**The user decides per source,** in the add-on's settings:
- **Off:** no webhook URL; the add-on polls when the Node is up, or does without.
- **Notify only:** the Relay stores a sealed "this source sent an event at this time" and **discards the body** without storing it. The Node fetches the details from the source when it's back. Many sources send only a change notice anyway.
- **Full payload:** the body is sealed and stored as above.

The setup screen says: "The Relay receives these events in the clear and encrypts them at once. Turn this off to keep them from passing through the Relay."
- **Webhook URLs are unguessable:** a random 128-bit token in the path, one per source. Rotating the token is a click away.
- **Signature checks happen on the Node,** after decryption, with the webhook secret the Node holds (for example GitHub's HMAC). The Relay can't check them, so it can't tell real deliveries from forged ones. Its limits (§5) bound what a forger can do.
- **On a self-hosted Relay,** the plaintext passes only through the user's own machine.

### 5. Abuse controls, without CAPTCHA

| Control | Default (hosted Relay) | Notes |
|---|---|---|
| Size per message | 64 KiB for open mailboxes; 1 MiB for restricted ones | Larger sync items are chunked |
| Messages per mailbox | Open: 1,000; restricted: 10,000 | Deliveries over the limit are refused until the owner fetches |
| Bytes per mailbox | Open: 10 MiB; restricted: 256 MiB | Same |
| Rate per sender address and mailbox | Open: 10 per minute, 100 per hour | Salted address hash, in memory only |
| Rate per sender address, all mailboxes | 600 per hour | Stops one address spraying many mailboxes |
| Delivery to restricted mailboxes | A valid, unexpired delivery grant | Anyone else is refused |
| Invitation mailboxes | One message, then closed | — |
| Close or rotate | Any time, signed by the owner | A flooded form or webhook gets a new mailbox |

- **The owner sees** each mailbox's counts and whether it's refusing deliveries, so a full mailbox is never a silent loss.
- **No CAPTCHA:** it hurts accessibility, and most CAPTCHA services send data to a third party. A browser proof-of-work for open mailboxes could be added later if abuse appears. ADR-0015 lists it as an option.
- **A self-hosted Relay** has the same controls, with limits its operator can change.

### 6. Retention

| What | Kept until |
|---|---|
| A message | It is acknowledged (deleted at once), or its TTL ends |
| TTL | 7 days by default; the mailbox owner can set 1 to 30 days. A form's messages also end 30 days after the form link expires or is closed (ADR-0015) |
| A mailbox | Closed by its owner, or 90 days with no fetch. Its owner is warned in the app before then |
| Push tokens | Until push is turned off or the mailbox closes |
| Network addresses | Never stored. The rate-limit hashes are in memory and rotate daily |
| Logs | Counts and errors only, for 14 days; no mailbox ids with addresses, no message content |
| Webhook plaintext | Only in memory, while it is being sealed |

- **"Delete all local data"** (and ADR-0014's wipe across devices) **closes every mailbox the user owns** on the Relay first, with signed requests, because the keys to close them are deleted with the rest. A mailbox that can't be closed (no network) is listed in the report with the date its messages expire.

### 7. Open source and self-hosting
- **The Relay is open source,** in this repository (`demios-desktop`, Apache-2.0), as a small Rust service, `dn relay run`. Its storage holds only ciphertext and per-mailbox limits.
- **Anyone can run one.** A user's Node can run its own Relay (`dn node` with the Relay switched on) on a public address. The app then uses that Relay for everything except official phone push (§4.2).
- **The app uses one Relay at a time,** shown in Settings › Node and Settings › Shared links: the Demios-hosted Relay, the user's own, or none.
  - **With none,** the features that need it say so, for example "Forms need a Relay to collect responses while your computer is off". Nothing quietly changes to another route.
- **The Relay is never required for the basics.** Chats, models, tools, a Node on the local network, and direct sync all work without it.

### 8. Cost model (hosted Relay)
The Relay is cheap to run because it deletes what it delivers:
- **Storage** is bounded by the per-mailbox caps (§5) times the number of active mailboxes. In steady state it's far below that, because devices fetch and acknowledge within minutes or hours. Expired messages and idle mailboxes are removed (§6).
- **Bandwidth** is each message in once and out once, plus chunked sync for devices that are rarely online together. Sync is the largest share, and it's bounded by the restricted caps.
- **Push** costs nothing per message from Apple or Google, but needs the developer accounts and the credentials kept in the operator's private configuration.
- **Compute** is small: TLS, signature checks, HPKE for webhooks only, and rate-limit counters.

Quotas on the hosted Relay keep the worst case bounded per mailbox. Whether some uses get larger quotas, and at what price, is a product decision outside this ADR. A self-hosted Relay costs its operator only their own machine and bandwidth.

### 9. GDPR roles
- **The Relay's operator** (DigiNote, Inc. for the hosted Relay) **processes ciphertext on the user's behalf**: a processor under Art. 28 for the mailbox contents and their metadata. Encrypted personal data is still personal data for the party that holds the key, so this ADR doesn't claim GDPR doesn't apply.
- **For webhooks with full payload or notify only** (§4.5), the operator **briefly processes plaintext** as a processor, in memory only. The user can turn this off per source.
- **The network addresses used for rate limits** are the operator's own processing, for the security of the service, held in memory as a daily salted hash.
- **Form respondents and invitees** aren't the Relay's users. The form or invitation owner is the controller for what they collect (ADR-0015 §9).
- **A self-hosted Relay** involves no Demios processing at all. Its operator is responsible for it.
- **The privacy policy gets a Relay section** in a separate, later PR for the owner to approve, together with ADR-0015's sharing section and the hosted-service terms. This section is not legal advice.

## Options considered

### One service or several

| Option | Notes |
|---|---|
| **One Relay of mailboxes for every store-and-forward need (chosen)** | One service to secure, one retention rule, one privacy section; per-purpose mailboxes keep uses apart |
| A separate service per feature (form drop point, push relay, sync relay, webhook receiver) | Simpler each, but four services, four sets of limits and four privacy sections |
| No store-and-forward; everything direct | Forms and webhooks would need the owner's computer on; the phone would miss approvals |

### Addressing

| Option | Linkability | Notes |
|---|---|---|
| **Per device and per purpose, by public key (chosen)** | Ids don't link a user's mailboxes; timing and addresses still can | Matches who holds which key |
| One mailbox per person | Every use linked by one id | Simpler; rejected for privacy |
| Accounts with a user name | Fully linked, and needs a password store | Rejected: no Demios account |

### Webhooks while the Node is down

| Option | Relay sees plaintext? | Notes |
|---|---|---|
| **Relay receives and seals at once, per-source switch with "notify only" (chosen)** | Briefly, in memory | Works with any webhook source |
| Third parties encrypt to the user's key | No | No mainstream service supports it |
| Only poll when the Node is up | No | Events while the Node is down arrive late; the user's choice via "Off" |
| A hosted Node receives them | Yes, and keeps them | Deferred (ADR-0014) |

### Abuse controls

| Option | Notes |
|---|---|
| **Per-mailbox caps, rate limits by hashed address, delivery grants for restricted mailboxes, close and rotate (chosen)** | No burden on senders; worst case is a full mailbox that the owner can see |
| CAPTCHA | Rejected: accessibility, and third-party data flows |
| Proof of work | Possible later for open mailboxes |
| Sender accounts | Rejected: respondents and webhook sources have none |

## Trade-offs
- **Metadata stays visible:** mailbox ids, sizes, timing and network addresses at the moment of a request. Per-purpose mailboxes, padding and not storing addresses reduce what that reveals. They don't remove it. A user who wants none of it with Demios runs their own Relay or uses none.
- **Webhooks pass through the Relay in the clear** for an instant. That's the price of receiving events while the Node is down. It's off per source, and "notify only" keeps bodies out.
- **Official phone push needs the hosted Relay.** That's a platform rule. A self-hosted setup can use the user's own Android push server, and on iOS approvals wait until the phone opens the app.
- **TTL and caps** mean a device offline for weeks catches up directly, not from the Relay. That's deliberate: the Relay holds messages in transit, not the user's data.
- **One service for many uses** makes it a bigger target. Per-purpose mailboxes, sealing before delivery, and holding no keys keep a breach to ciphertext plus the metadata above.

## Consequences
- **ADR-0014** points its optional relay and its push at this ADR. A Node can run its own Relay.
- **ADR-0015** uses a Relay mailbox per form link for responses, instead of defining its own store.
- **`dn`** gains `relay run`. **`dn-sync`** gains mailbox delivery and chunking. A shared crate (or module) holds the Relay client: sealing, signing, grants and fetching.
- **The app** shows which Relay is in use, each mailbox's state, and the webhook setting per source, and closes mailboxes in "Delete all".
- **Dependencies:** HPKE, Ed25519 and an HTTP server, the same ones ADR-0015 needs, each to be approved by the lead and recorded in `docs/dependencies.md`.

## Action items (to be filed as issues)
1. Relay service: mailboxes, signed owner requests, delivery grants, limits, TTL and deletion.
2. Relay client: sealing, signing, chunking and fetching, shared by the app and the Node.
3. Push wake-ups through the hosted Relay, and UnifiedPush or ntfy for self-hosted Relays.
4. Sync through mailboxes when devices aren't online together (`dn-sync`).
5. Webhook receiver with per-source Off, Notify only and Full payload, and unguessable, rotatable URLs.
6. Self-hosted Relay on the Node, and the app's choice of Relay.
7. "Delete all" closes the user's mailboxes, and reports any it couldn't close.
8. Threat model for the Relay (extends ADR-0009): metadata, webhook plaintext, abuse, and a breach of the operator.
9. Privacy-policy section and hosted-service terms, for owner approval.
