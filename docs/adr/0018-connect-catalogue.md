# ADR-0018: Connect: a curated catalogue of tool servers, with no connector code to maintain

**Status:** Accepted (owner, 2026-11-26) · **Date:** 2026-11-26 · **Deciders:** project owner, lead · **Extends:** [ADR-0006](0006-tools-mcp-capabilities.md), [ADR-0007](0007-local-storage-and-secrets.md), [ADR-0009](0009-security-baseline.md), [ADR-0014](0014-demios-node.md)

## Context
People want the app to work with the services they already use: mail, calendar, documents, issue trackers, chat. The app already adds any MCP tool server: a program on the computer or a remote HTTPS server, with sign-in in the browser (ADR-0006), and every tool goes through risk levels and approvals.

What's missing is the easy path: finding a good server for a service, trusting it, and signing in without reading setup guides. There are two ways to provide it.
- **Build and host our own connectors,** with OAuth apps registered by Demios. Then we maintain code for every service API, and we hold an identity that providers review. Reading Gmail, for example, is a restricted scope that needs a paid security assessment every year.
- **Curate what already exists.** Many services now run their own hosted MCP servers with their own sign-in, and well-maintained open-source servers cover most of the rest.

The owner asked for the second: no connector code to maintain. Demios registers OAuth apps only where one-click sign-in can't work without one: Google and Microsoft.

## Decision

### 1. Connect is a catalogue, not a set of connectors
- **Connect** is a list in **Settings › Tools › Connect** of reviewed tool servers, one per service, each with **Connect**, which adds it as an ordinary tool server. There is no special runtime: every tool keeps its risk level, approvals and history (ADR-0006).
- **The catalogue is data,** a signed JSON file shipped with the app and updated with it. Each entry names the service, the server and its kind (below), the pinned version or URL, the scopes asked for, the tools' default risk levels, a licence and a "last reviewed" date.

### 2. Four kinds of entry, in this order of preference
1. **Vendor-hosted remote server.** The service runs the MCP server and its own sign-in (OAuth with dynamic client registration or the vendor's public client). Demios holds no client secret, and the service's data goes from the service to the app without touching Demios. The vendor maintains it.
2. **Pinned open-source server.** For a service without an official server, a widely used open-source MCP server, pinned to a reviewed release by checksum and run locally (or on the user's Node). Updating the pin is a reviewed change to the catalogue, with the same checks as a new dependency (`docs/dependencies.md`). We review, we don't write.
3. **Google and Microsoft through Demios-registered OAuth apps.** Their mail, calendar and file APIs need a registered OAuth client, and there's no self-registration. For one-click sign-in, Demios registers one public desktop client with Google and one with Microsoft (a verified publisher), asking only for the scopes the catalogue entries use. Sign-in runs in the browser with PKCE, and the tokens go to the user's device or Node (system keychain or server secret store, ADR-0007/ADR-0014). Demios's servers never receive the tokens or the data: the client id only identifies the app to Google and Microsoft.
4. **An organisation's own OAuth apps.** An organisation can register its own app in its Google Workspace or Microsoft tenant, which needs no Google or Microsoft verification for internal use, and its admin sets that app for every member's devices. The organisation then controls the app, its scopes and who may use it, and the Demios-registered app isn't used for its members. A person may do the same for themselves (guided setup below).

Other services that need a registered client and have neither an official server nor self-registration get one only through a later change to this ADR.

### 3. Guided setup for an own OAuth app (organisations, and anyone who prefers it)
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
| A. Build and host our own connectors | High: one integration per API | Demios's OAuth apps for every service, provider reviews and yearly assessments | Lowest |
| **B. Curated catalogue (chosen)** | **Low: review and pin** | **Demios-registered apps for Google and Microsoft only; organisations may use their own** | One click |
| B′. Curated catalogue, own OAuth apps only | Low | None | About 15 minutes once per provider; many people stop at the cloud console |
| C. No catalogue, users add servers themselves | None | None | Highest; most users never find a good server |

## Trade-offs
- **Demios's Google app needs Google's verification,** and reading Gmail is a restricted scope with a paid security assessment every year. That cost buys one-click sign-in. Microsoft needs a verified publisher, which is free.
- **The Demios apps' client ids are public by nature** (desktop clients can't keep a secret). Abuse protection relies on PKCE, the registered redirect and the provider's own consent screen, as for any desktop app.
- **An own OAuth app takes setup** (organisations and people who choose it): the guided setup and clear failure messages keep that small.
- **Quality depends on others.** A vendor can change or retire its server, and an open-source project can stall. The review rules and the "no longer reviewed" mark handle that openly.
- **Vendor servers see the data they serve.** That's the service's own data on the service's own server, and the catalogue says so per entry.

## Check before building (may have changed since this was written)
- Which services run official remote MCP servers today, and how their sign-in works (dynamic client registration or a public client).
- Google's verification and restricted-scope assessment for the Demios app (requirements, cost, renewal), its rules for an organisation's internal apps and for a person's own app (Testing token expiry); Microsoft's publisher verification.
- The open-source candidates for Gmail, Google Calendar and Outlook: licence, release activity and scopes.

## Consequences
- The app gains the Connect list, the catalogue file and its signature check, and the guided OAuth setup screens. Tool servers, approvals and the ledger are unchanged.
- `docs/dependencies.md` lists every pinned server, as for any dependency.
- New screens are designed in the design reference first (design standard §8).

## Action items
1. Catalogue format, signing and update path; the first entries from vendor-hosted servers.
2. Connect list and the one-click add for vendor servers.
3. Demios-registered Google and Microsoft apps: registration, verification, scopes per catalogue entry, and the privacy-policy section that Google's verification asks for (owner).
4. An organisation's own apps, set by its admin for all members; the guided setup with its failure messages.
5. Pinned open-source servers for mail and calendar, reviewed and listed.
6. Connect on the Node.
