# ADR-0007: Local storage, knowledge index and secrets

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner

## Decision
- **SQLite** (via `sqlx` or `rusqlite`, decided in T-CORE-02) for conversations, messages, artifacts + versions, skills config, ledger, Twin memory. Migrations in `dn-store`. Optional at-rest encryption via SQLCipher with a key held in the OS keychain (required for the Twin's memory).
- **Files** (attachments, models) under the OS app-data dir; models in a shared cache dir with SHA-256 manifests.
- **Knowledge index** (`dn-knowledge`): SQLite FTS5 for keyword + an embedded vector index (sqlite-vec or LanceDB — spike T-KNO-01); local embedding model through `dn-inference`; hybrid rank (Demios used vector + BM25 + rerank). No Qdrant, no server.
- **Secrets** (BYOK keys, MCP OAuth tokens): OS keychain via `keyring`; never in SQLite, never in logs. Replaces Demios's Mongo + Fernet with weak fallback.

## Consequences
Single-user, zero-setup. The team product syncs selected tables (ADR-0008), so schema must carry stable UUIDs and `updated_at`/HLC timestamps from day one.
