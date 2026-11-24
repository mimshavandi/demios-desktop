# ADR-0008: Team server and sync

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner (D-009)

## Context
Product 3 = open-source desktop + a central service connecting team members. Demios's org model and OIDC flow are worth porting; its session scheme is replaced by a new token design; its realtime layer is notification-only without replay, so sync is designed fresh.

## Decision
- **One Rust binary** (`dn-team-server`, axum + tokio) + **Postgres** (SQLite allowed for ≤10 users). Docker image and plain binary. No NATS, no Kubernetes required.
- **Identity:** users, orgs, roles owner/admin/member, email invites, audit log (port of Demios org model). Device login → device-bound refresh token + 15-min access token; per-user revocation counter (Demios `passcode` idea). OIDC SSO later (port `enterprise_sso.py` flow).
- **Sync:** desktop stays local-first. Users opt specific *spaces* (projects) into sync. Each syncable document (artifact versions, project files, skills, notes) is a CRDT doc (**Automerge** or **Yrs/Yjs** — spike T-TEAM-02); server stores ops with a per-space sequence number; clients resume from a cursor; offline queue on the client. Envelope ported from Demios (`event_id`, `schema_version`, `type`, `created_at`, `correlation_id`). Transport: WebSocket with short-lived ticket + authorize-on-join (Demios realtime pattern).
- **Shared models:** org can register shared remote endpoints / BYOK keys server-side; the server proxies calls so keys never reach clients. Optional team inference box via the Edge job relay protocol (ported envelope, hashed tokens).
- **E2E encryption** of synced content: open question (D-010) — strong privacy story vs. server-side search/agents.

## Options considered

| Option | Offline | Conflict handling | Ops burden |
|---|---|---|---|
| **A. CRDT docs + op log on one server** | Full | Automatic merge | Low |
| B. Server-authoritative REST (Demios style) | Poor | Last-write-wins | Low |
| C. Peer-to-peer (no server) | Full | CRDT | No admin/identity story |

## Action items
T-TEAM-01 server skeleton + auth, T-TEAM-02 CRDT spike, T-TEAM-03 sync protocol, T-TEAM-04 shared model proxy.
