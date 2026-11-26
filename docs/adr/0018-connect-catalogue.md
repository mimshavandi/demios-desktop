# ADR-0018: Connect: a curated catalogue of tool servers, with no connector code or identity run by Demios

**Status:** Proposed · **Date:** 2026-11-26 · **Deciders:** project owner, lead · **Extends:** [ADR-0006](0006-tools-mcp-capabilities.md), [ADR-0007](0007-local-storage-and-secrets.md), [ADR-0009](0009-security-baseline.md), [ADR-0014](0014-demios-node.md)

## Context
People want the app to work with the services they already use: mail, calendar, documents, issue trackers, chat. The app already adds any MCP tool server: a program on the computer or a remote HTTPS server, with sign-in in the browser (ADR-0006), and every tool goes through risk levels and approvals.

What's missing is the easy path: finding a good server for a service, trusting it, and signing in without reading setup guides. There are two ways to provide it.
- **Build and host our own connectors,** with OAuth apps registered by Demios. Then we maintain code for every service API, and we hold an identity that providers review. Reading Gmail, for example, is a restricted scope that needs a paid security assessment every year.
- **Curate what already exists.** Many services now run their own hosted MCP servers with their own sign-in, and well-maintained open-source servers cover most of the rest.

The owner asked for the second: no connector code to maintain, and no identity for Demios to run.

## Decision

### 1. Connect is a catalogue, not a set of connectors
- **Connect** is a list in **Settings › Tools › Connect** of reviewed tool servers, one per service, each with **Connect**, which adds it as an ordinary tool server. There is no special runtime: every tool keeps its risk level, approvals and history (ADR-0006).
- **The catalogue is data,** a signed JSON file shipped with the app and updated with it. Each entry names the service, the server and its kind (below), the pinned version or URL, the scopes asked for, the tools' default risk levels, a licence and a "last reviewed" date.

### 2. Three kinds of entry, in this order of preference
1. **Vendor-hosted remote server.** The service runs the MCP server and its own sign-in (OAuth with dynamic client registration or the vendor's public client). Demios holds no client secret, and the service's data goes from the service to the app without touching Demios. The vendor maintains it.
2. **Pinned open-source server.** For a service without an official server, a widely used open-source MCP server, pinned to a reviewed release by checksum and run locally (or on the user's Node). Updating the pin is a reviewed change to the catalogue, with the same checks as a new dependency (`docs/dependencies.md`). We review, we don't write.
3. **Bring your own OAuth app.** Where a pinned server needs an OAuth client (Google and Microsoft mail and calendar today), the user creates their own app in Google Cloud or Microsoft Entra and enters its client id in a guided setup. Sign-in runs in the browser with PKCE; any client secret the provider issues to a desktop client is kept in the system keychain (ADR-0007), never in settings or logs. Demios is not the OAuth client and never sees the tokens' use.

A Demios-registered OAuth app (one-click sign-in for Google or Microsoft) is **not part of this decision.** It stays possible later, as an extra entry, only if users ask for it.

### 3. Guided setup for your own OAuth app
- A step-by-step screen per provider: which console page to open, which app type to pick (desktop), which scopes to add, which redirect to allow, with Copy buttons and a check of the entered client id.
- It says plainly what the provider will show. For Google: the app must be switched from **Testing** to **In production**, or its sign-in expires after 7 days; the user then confirms the "Google hasn't verified this app" screen once, for their own app.
- A failed sign-in names the cause (wrong redirect, missing scope, app still in Testing) and the step to fix it.

### 4. Review rules for the catalogue
- An entry needs: a maintained source (vendor, or an open-source project with recent releases and a security contact), a licence the app may point users to, the scopes listed and kept to what its tools need, and a tool list whose risk levels we've checked.
- Read-only tools default to asking once per chat; anything that sends, writes or deletes asks every time, as today.
- An entry that stops meeting the rules is marked "no longer reviewed" in the next catalogue and offered for removal; it's never removed from someone's setup silently.

### 5. Where it runs
- On the desktop app, or on the Node so the user's own scheduled jobs can use it while the computer is off (ADR-0014). The Node keeps tokens in its server secret store.

## Options considered
| Option | Maintenance | Identity | User effort |
|---|---|---|---|
| A. Build and host our own connectors | High: one integration per API | Demios's OAuth apps, provider reviews and yearly assessments | Lowest |
| **B. Curated catalogue (chosen)** | **Low: review and pin** | **None for vendor servers and own-app setups** | One click for vendor servers; about 15 minutes once for own OAuth apps |
| C. No catalogue, users add servers themselves | None | None | Highest; most users never find a good server |

## Trade-offs
- **Own OAuth apps take setup.** Some people will stop at the cloud console. The guided setup and a clear failure message keep that small; a Demios-registered app remains possible later.
- **Quality depends on others.** A vendor can change or retire its server, and an open-source project can stall. The review rules and the "no longer reviewed" mark handle that openly.
- **Vendor servers see the data they serve.** That's the service's own data on the service's own server, and the catalogue says so per entry.

## Check before building (may have changed since this was written)
- Which services run official remote MCP servers today, and how their sign-in works (dynamic client registration or a public client).
- Google's current rules for unverified apps used by their owner, including the Testing token expiry and the user limit, and Microsoft's for personal-account app registrations.
- The open-source candidates for Gmail, Google Calendar and Outlook: licence, release activity and scopes.

## Consequences
- The app gains the Connect list, the catalogue file and its signature check, and the guided OAuth setup screens. Tool servers, approvals and the ledger are unchanged.
- `docs/dependencies.md` lists every pinned server, as for any dependency.
- New screens are designed in the design reference first (design standard §8).

## Action items
1. Catalogue format, signing and update path; the first entries from vendor-hosted servers.
2. Connect list and the one-click add for vendor servers.
3. Guided own-OAuth-app setup for Google and Microsoft, with the failure messages.
4. Pinned open-source servers for mail and calendar, reviewed and listed.
5. Connect on the Node.
