# ADR-0006: Tools — MCP + capability/approval layer

**Status:** Proposed · **Date:** 2026-11-24 · **Deciders:** project owner

## Context
Demios has a strong capability contract (AppCapability, preflight, CapabilityError, `run_side_effects` ledger, risk/approval) but in practice only Gmail executes, and there is no MCP client. MCP is now the standard way for desktop AI apps to get tools.

## Decision
- **MCP is the only external tool protocol** (client over stdio and Streamable HTTP, via the `rmcp` crate — dependency to confirm). OAuth for remote MCP servers handled by the app; tokens in the keychain.
- Every tool (MCP, builtin, artifact) is wrapped as a **Capability**: `{id, source, input_schema, output_schema, risk: read|low_write|external_write|destructive|sensitive, idempotency_key_field, retry_policy}` (Demios fields; risk enum from Demios's `approval_policy.py`). MCP tool annotations (`readOnlyHint`, `destructiveHint`) seed the risk; user can override.
- **Preflight** statuses ported: `ready | needs_input | needs_connection | scope_missing | approval_required | blocked`.
- **Approval:** per-capability policy `always_ask | ask_once_per_conversation | auto`; default `auto` only for `read`.
- **Side-effect ledger** in SQLite with Demios semantics (claim → running → succeeded/failed; replay returns cached output; running blocks; auto-retry only reads and idempotent writes).
- **Errors:** `CapabilityError` envelope (user_message vs developer_message, retryable, reconnect_required).
- Never fall back silently to another connection (Demios rule kept).

## Consequences
Product 1 gets a large tool ecosystem for free. Add-ons reuse the same ledger and approval queue — fixing Demios's gap where approvals weren't bound to the ledger.

## Action items
T-CON-01 MCP client, T-CON-02 capability wrapper + risk, T-CON-03 approval UI + ledger, T-CON-04 MCP OAuth.
