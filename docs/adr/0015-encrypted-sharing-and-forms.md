# ADR-0015: Sharing artifacts through encrypted links, and forms that collect encrypted responses

**Status:** Proposed · **Date:** 2026-11-25 · **Deciders:** project owner, lead · **Extends:** [ADR-0004](0004-artifacts-runtime.md), [ADR-0009](0009-security-baseline.md), [ADR-0014](0014-demios-node.md), [ADR-0016](0016-demios-relay.md) · **Issue:** #204 · **Threat model it builds on:** [artifact sandbox](../security/artifact-sandbox.md)

## Context
Artifacts (ADR-0004) live only in the app today. The owner decided that a user can **share an artifact through a link**. That includes **forms that collect responses while the owner's computer is off**.

That needs something on the internet that serves the shared artifact, and something that holds responses until the owner comes back. The second is a mailbox on the **Demios Relay** (ADR-0016), the general encrypted store-and-forward service. This ADR doesn't define its own store. Demios has no server and no account, and the rest of the product is built so Demios can't read user data. This ADR keeps it that way:
- **The share host stores ciphertext only.**
- The key to an artifact travels **in the link**, not to the host.
- Responses are encrypted **in the respondent's browser**, to the owner's key.

**Be honest about the limits:**
- **Anyone who has the link can open the artifact.** The link *is* the key.
- **The viewer's code comes from the share host.** A malicious or compromised host could serve code that reads the key out of the link. Open source, published hashes and a small viewer reduce that risk. In a normal browser, nothing can remove it (§8).
- **A shared artifact runs in the respondent's browser, not in the app.** The app's engine-level controls, such as WebRTC switched off in the webview, aren't available there. The same in-page guards are.

This ADR decides the design. It doesn't implement it.

## Decision

### 1. The share host
- **The share host is a static site on its own domain**, for example `share.demios.io`, separate from `demios.io` and from the app. It serves:
  - the **viewer**: static HTML, JS and CSS, the same for every link;
  - **stored ciphertext**, one blob per link;
  - **one small write endpoint** for uploading and managing blobs. It's a Rust service whose only storage is ciphertext plus per-link metadata.

  Form responses don't go to the share host. They go to a Relay mailbox (§5, ADR-0016).
- **The frame shell** that runs the artifact is served from a **second origin**, on a separate registrable domain, so artifact content is never same-site with `demios.io` or the viewer. This is the share host's version of the app's separate artifact origin (threat model C2).
- **Neither origin sets cookies or uses storage.** There's no login.
- **What the host stores per link, in the clear:**
  - a random id;
  - the blob's size and its padded size bucket;
  - created, updated and expiry times;
  - the management public key (§3);
  - for forms: the response limit, response count and whether the link is closed.

  It stores **no titles, names, content or IP addresses** with a link.
- **Who may create links on the hosted service** (quotas, a creation credential) is decided outside this ADR. Links served by the user's own Node (§6) need no credential.

### 2. Encryption in the app, key only in the URL fragment
- **When the user shares an artifact,** the app:
  1. builds a **bundle**: the artifact's kind, content and version, its title, and for a form, the form settings (§5);
  2. pads it to a size bucket, so the stored size reveals less;
  3. encrypts it with **AES-256-GCM** under a fresh random 256-bit **content key**;
  4. uploads only the ciphertext.
- **The link is `https://<share host>/s/<id>#k=<content key, base64url>`.**
  - Browsers never send the part after `#` to a server, so the host never sees the key.
  - With `Referrer-Policy: no-referrer` it doesn't leak through the Referer header either. A fragment isn't sent there anyway.
- **The viewer decrypts in the browser with WebCrypto** (AES-GCM is built in). There's no third-party crypto library in the viewer.
- The share sheet says what the link means, in plain words: "Anyone with this link can open it. Demios can't." It also says the link stays in the browser history of everyone who opened it.

### 3. Expiry, revoke, password and links that update in place
- **Every link has a management key pair** (Ed25519), made in the app when the link is created. The public key goes to the host.
  - The private key stays in the app's keychain, and syncs to the user's Node if they have one (ADR-0014). Every change to a link is a request **signed** with it. There are no bearer tokens.
  - Settings › Shared links lists every link with its state, expiry, and for forms, response counts.
- **Expiry is required.** The user picks it when sharing (default 30 days, maximum 1 year). The host deletes the blob when it expires, and serves "This link has expired" from then on.
- **Revoke** deletes the blob at once (signed request). The link then shows "This link was turned off by its owner".
- **Optional password:**
  - The content key is derived from both the fragment key **and** the password. The password goes through PBKDF2 with SHA-256 in WebCrypto (600,000 iterations, a per-link salt), and HKDF combines the two.
  - It protects against a **leaked link**, not against the host. The host can't open the blob without the fragment key anyway.
  - There's no recovery. A forgotten password means sharing again.
- **Links update in place.** When the artifact gets a new version, the user can update the link (signed upload of a new blob, encrypted with the same key). The link stays the same.
  - The viewer shows "Updated <date>".
  - Updating is a deliberate action from the share sheet. A new version doesn't update the link by itself.
  - Old blobs are deleted. The host keeps only the latest one.
- **"Delete all local data"** (and ADR-0014's wipe) **revokes every link first**, because the management keys are deleted with the rest. A link that can't be revoked (no network) is listed in the report with its expiry date.

### 4. The viewer: the same locked-down sandbox, no network
The viewer renders an artifact exactly as the app does: the same frame shell, bridge and runtimes, from `ui/artifact-sandbox`, in a `sandbox="allow-scripts"` iframe with an opaque origin (threat model C1), and the same frame protocol (C5).

**A shared artifact never has network access.** The per-artifact network toggle doesn't exist on the share host.

**Viewer page** (`share.<domain>`), sent as headers:
```
Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self';
  connect-src 'self' https://<relay origin>; frame-src https://<frame origin>/frame/; form-action 'none'; base-uri 'none';
  object-src 'none'; frame-ancestors 'none'; require-trusted-types-for 'script';
  trusted-types share-viewer; upgrade-insecure-requests
Referrer-Policy: no-referrer
X-Content-Type-Options: nosniff
Cross-Origin-Opener-Policy: same-origin
Permissions-Policy: camera=(), microphone=(), geolocation=(), clipboard-read=(), clipboard-write=(),
  payment=(), usb=(), serial=(), bluetooth=(), display-capture=()
Strict-Transport-Security: max-age=63072000; includeSubDomains
```
- `connect-src` lets the viewer fetch the blob from its own host and deliver form responses to the Relay (ADR-0016), and nowhere else. The artifact frame itself can still connect to nothing.
- `frame-src` also governs the artifact frame's navigations (C4).

**Frame shell** (`<frame origin>/frame/`): the app's strict policy (C3), unchanged:
```
Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline';
  img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none';
  child-src 'none'; worker-src 'none'; object-src 'none'; manifest-src 'none'; form-action 'none';
  base-uri 'none'; frame-ancestors https://share.<domain>
Referrer-Policy: no-referrer
X-Content-Type-Options: nosniff
```
- The host also writes the same policy as the first `<meta>` of the artifact document, as in the app.
- The iframe gets `referrerpolicy="no-referrer"` and `allow=""`.
- **WebRTC and DNS side channels.** A normal browser can't have WebRTC switched off by a page, so the app's engine-level fix (shell requirement S4) has no equivalent here. The bridge's in-realm guards apply (C6): RTC constructors deleted, nested frames removed. They are defence in depth, not a boundary. This is residual risk **R-S1**: a hostile artifact could leak what it shows, or what a respondent types into it. The share sheet shows the artifact's kind and the form's fields before sharing, so the owner shares what they've seen.

### 5. Forms: one `submit` capability, responses sealed to the owner
- **An artifact may declare one capability: `submit`.** Nothing else can be declared.
  - The bridge then exposes `demios.submit(answers)` to the artifact. It **never** exposes `fetch`, and the frame's CSP still blocks every connection.
  - `submit` posts the answers to the viewer page, through the frame protocol.
- **The viewer checks each submission:**
  - a flat JSON object of strings, numbers and booleans;
  - at most 100 fields and 64 KiB;
  - field names that the form settings declared.

  It then shows a **review panel the viewer draws itself**, outside the frame: every field as plain text, the collector's name (§7) and a **Send** button.
  - **Only the respondent's click on that button sends anything.** An artifact can't submit silently, or submit something other than what the respondent sees.
  - One submission per click. The panel locks for a few seconds after each one.
- **Sealing in the respondent's browser:**
  - Each form link has its own **response key pair** (X25519), made in the app. The private key stays with the owner (keychain, synced to their Node if they have one). The public key is inside the encrypted bundle (§2), so the host can't swap it without breaking decryption.
  - Each response is encrypted with **HPKE in base mode** (RFC 9180: DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, AES-256-GCM): a sealed box, readable only with the owner's private key.
  - WebCrypto provides each of these building blocks. A browser without X25519 in WebCrypto gets "This browser can't send this form securely", and nothing is sent. There's no weaker fallback.
  - A response carries a random id, so the owner can drop duplicates.
- **Each form link has its own open mailbox on the Relay** (ADR-0016 §4.1), addressed by the form's public key. The mailbox address is inside the encrypted bundle, next to the key. The viewer delivers each sealed response there. The Relay stores **only ciphertext**.
- **The owner's app or Node fetches responses** whenever it's online:
  1. It sends a signed request for the form's mailbox.
  2. It decrypts the responses and stores them with the artifact, where they're listed and can be exported.
  3. It **acknowledges** them, and the Relay **deletes** them.
  - Responses that are never fetched are deleted 30 days after the link expires or is closed. The share sheet says so.
- **What the Relay can still do:** drop, delay or replay responses. It can't read or forge them. Replays are dropped by response id. Missing responses can't be detected, and this ADR doesn't claim they can. What else the Relay can see (timing, sizes, network addresses) is listed in ADR-0016 §3.
- **In the app itself,** an artifact with `submit` shows the same review panel. Sending there does nothing but show "Responses are collected only through a shared link".

### 6. Self-hosted: the user's Node as share host and Relay
- **A user with a Demios Node** (ADR-0014) can serve links from it instead of from the Demios host. The Node serves the **same viewer and frame shell, with the same headers**, from the user's own address, and runs its own Relay (ADR-0016 §7) for the form mailboxes.
  - Responses then never leave hardware the user controls.
  - Links point at the user's address. The viewer and the Relay are on that same origin, so `connect-src` there is `'self'`.
- The Node must be **reachable from the internet** for respondents. That means a domain with a certificate, and a forwarded port or the user's own tunnel. The setup screen says so. It doesn't hide the requirement.
- **The Node serves only the share paths on that public address.** The paired-device API (ADR-0014 §2) never listens on it.

### 7. Abuse controls without CAPTCHA
The Relay's own controls apply to every form mailbox (ADR-0016 §5). A form sets its mailbox's limits tighter:

| Control | Default | Notes |
|---|---|---|
| Responses per link | 100 (owner can set up to 1,000, the Relay's cap for open mailboxes) | The link then shows "This form is full" |
| Size per response | 64 KiB | Checked by the viewer and by the Relay |
| Rate per address and link | 10 per hour | Enforced by the Relay, which uses the address **in memory only**, as a salted hash with a salt that changes daily, and never stores it with a response |
| Expiry | Required (§3) | Unfetched responses are deleted 30 days after expiry |
| Close | Any time, signed | "This form is closed"; responses already in the mailbox can still be fetched |
| Total stored per link | 10 MiB of ciphertext | New responses are refused until the owner fetches |

If a form fills up with junk, the owner can see it, close the link or raise the limit. That's an owner's choice, not a silent one.

### 8. The respondent sees who collects the data
- **The viewer draws a notice above the form, outside the frame,** so the artifact can't hide or fake it: "**<name>** collects your answers to this form. They are encrypted in your browser, and only <name> can read them. Demios hosts the encrypted data and can't read it."
  - `<name>` is what the owner entered when sharing, and is **required for forms**. It's marked as not verified.
  - The owner can add a contact or privacy link, shown next to it.
- A link to the share host's privacy note sits below the notice. It explains the encryption, the limits, and that the viewer's code comes from the host (below).
- **Trust in the viewer's code, stated plainly:**
  - The viewer is small, built reproducibly from the open-source repository, and published with the hashes of each release.
  - All scripts load from the viewer's own origin, with Subresource Integrity.
  - A respondent's browser still runs what the host serves. A compromised host could change it, and that's the residual risk **R-S2**.

### 9. GDPR roles
- **The owner is the controller** for the responses their form collects, and for the artifact they share. They choose what to ask and who to share with.
- **DigiNote, Inc., as operator of the share host and the hosted Relay,** processes the ciphertext, per-link metadata and form mailboxes on the owner's behalf, and can't read the content. Encrypted personal data is still personal data for the party that holds the key, so this ADR doesn't claim that GDPR doesn't apply.
- **The addresses used for rate limits** (§7) are the operator's own processing, for security. They're held in memory, and only as a daily salted hash.
- **The privacy policy needs a section on sharing and forms.** It's a separate, later PR for the owner to approve, together with terms for hosted links and a data processing agreement for business users. A Node-hosted link involves no Demios processing at all. This section is not legal advice.

### 10. Where the code lives
- **The viewer, the frame shell and the share endpoint live in this repository** (`demios-desktop`, Apache-2.0), next to `ui/artifact-sandbox`, which they reuse. Same code, same escape tests, same review.
- **Open source is recommended for trust.** A respondent who doesn't use Demios has to trust the viewer's code. The only way to make that trust checkable is to publish it and let anyone rebuild it.
- Deployment configuration and credentials for the hosted service live in a private operations repository. No secrets come into this one.

## Options considered

### Where the key lives

| Option | Host can read? | Usability | Notes |
|---|---|---|---|
| **Key in the URL fragment (chosen)** | No | One link, nothing to type | The link is the key; it stays in browser history |
| Key stored on the host, link is an id | Yes | Same | Rejected: Demios could read every shared artifact |
| Password only, no key in the link | No, but open to guessing offline | A password to share separately | Kept as an **optional extra** on top of the fragment key |
| Key exchanged per viewer (accounts or public keys) | No | Every viewer needs Demios | Rejected for public links; possible later for sharing with paired people |

### How responses are sealed

| Option | In the browser | Notes |
|---|---|---|
| **HPKE base mode (RFC 9180), with WebCrypto building blocks (chosen)** | No third-party crypto in the viewer | A standard; X25519 in WebCrypto is needed, with no fallback for browsers that lack it |
| libsodium `crypto_box_seal` | Needs libsodium.js (WASM) in the viewer | Well-known, but adds a large dependency to code respondents must trust |
| age | Needs a JS port | Fine on the owner's side, but adds a dependency in the browser |
| Encrypt only in transit (TLS) | — | Rejected: the host could read responses |

### Where responses wait

| Option | Works with the owner's computer off | Who holds ciphertext | Notes |
|---|---|---|---|
| **A mailbox on the Demios-hosted Relay (chosen default)** | Yes | Demios (ciphertext only) | One general service (ADR-0016) with its own abuse controls and privacy section |
| **The Relay on the user's own Node (chosen option)** | Yes, while the Node runs | The user | Needs a public address; no Demios processing |
| A store of its own on the share host | Yes | Demios (ciphertext only) | Rejected: a second store-and-forward service next to the Relay |
| Peer-to-peer to the owner's app (WebRTC) | No | Nobody | Rejected: the owner's computer would have to be on |
| A third-party form service | Yes | The service, often in the clear | Rejected: data leaves the encrypted design |

### Abuse controls

| Option | Accessibility and privacy | Notes |
|---|---|---|
| **Limits per link, size, rate, expiry, close (chosen)** | No extra burden on respondents | A full form is the worst case; the owner can see it and act |
| CAPTCHA | Hurts accessibility; most send data to a third party | Rejected |
| Proof of work in the browser | Costs battery, slow on old phones | Could be added later per link if spam appears |
| Respondent accounts | Excludes most respondents | Rejected |

### Where the share host's code lives

| Option | Trust | Notes |
|---|---|---|
| **In demios-desktop, reusing `ui/artifact-sandbox` (chosen)** | Public, reproducible, same review | One sandbox, one set of escape tests |
| A separate public repository | Public | The sandbox would be duplicated or published as a package |
| A private repository | Respondents can't check the code | Rejected |

## Trade-offs
- **The link is the key.** That makes sharing easy and keeps Demios blind. It also means a forwarded link is a shared artifact, and a link in someone's browser history stays readable until it expires or is revoked.
- **Trust in the viewer's code** can't be removed in a normal browser. Open source, reproducible builds and published hashes make it checkable, not automatic.
- **A shared artifact runs without the app's engine-level hardening** (R-S1). The owner sees what they share, a form can only send through the review panel, and nothing has network access. But a hostile artifact could still try side channels in some browsers.
- **The Relay can drop responses** without being detected. Detecting that would need signed respondent identities, which forms are meant to do without.
- **Required expiry and per-link limits** make some uses (a permanent public form) less convenient. They bound what Demios stores, and what an abuser can do with one link.

## Consequences
- **`dn-artifacts`** gains a capability declaration on artifacts (only `submit`), form settings, and stored responses per artifact.
- **`ui/artifact-sandbox`** gains the `submit` bridge call and the viewer page with its review panel and notice. The escape suite is extended to run against the viewer in Chromium, Firefox and WebKit.
- **The app** gains a share sheet, Settings › Shared links (list, update, revoke, responses), response fetching, and link revocation in "Delete all".
- **The Node** (ADR-0014) gains an optional public share server (viewer and frame shell, with the same headers), next to its own Relay (ADR-0016).
- **New dependencies**, each to be approved by the lead and recorded in `docs/dependencies.md`: AES-GCM, HPKE and Ed25519 implementations on the Rust side, and the share endpoint's HTTP server (`axum`, pre-approved for a server use; this use still goes through the lead). The viewer uses WebCrypto only.
- **The privacy policy** gets a sharing-and-forms section in a separate PR for the owner to approve (§9).

## Action items (to be filed as issues)
1. Bundle format, padding buckets, the link format, and AES-GCM in the app.
2. Share endpoint: blob upload, signed management (update, revoke) and expiry. Form mailboxes on the Relay, with the form's limits (ADR-0016).
3. Viewer page and frame shell on two origins, with the headers above; escape suite run against both in three browser engines.
4. `submit` capability: declaration, bridge call, review panel, HPKE sealing in the viewer, response fetch, decrypt and acknowledge in the app.
5. Settings › Shared links, and revocation in "Delete all".
6. Self-hosted share server on the Node.
7. Threat model for sharing (extends the artifact-sandbox threat model): R-S1 side channels, R-S2 viewer code trust, and abuse. The Relay's own threat model is in ADR-0016.
8. Privacy-policy section and hosted-link terms, for owner approval.
9. Reproducible viewer build, published hashes, and Subresource Integrity.
