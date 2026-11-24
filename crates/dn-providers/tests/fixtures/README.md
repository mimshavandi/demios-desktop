# Provider HTTP fixtures

Response bodies the fixture server in `tests/common` replays to the real `reqwest`
client, so the adapters' HTTP, SSE and JSON handling run end to end with no network and
no keys.

- `*.sse`: streaming chat responses (`text/event-stream`), one file per scenario.
- `*.json`: non-streaming responses (`/models`, error bodies).

**Provenance: doc-derived, not captured.** These files were written by hand from each
provider's API documentation. They use real field names, event order and chunking (for
example, tool-call arguments split across deltas), with ids, signatures and key fragments
set to `REDACTED`. They are **not** recordings of real API traffic. They test our parsing
against the documented format; they don't prove the live APIs still match it.

**Recording real captures** (once keys are available): `tests/live.rs` is `#[ignore]` and
reads keys from env vars. With `DN_RECORD_FIXTURES=1` it routes the adapter through a local
proxy that forwards to the real API and writes `<provider>/text.recorded.sse`. The key and
every `id`, `responseId`, `signature` and `thoughtSignature` are replaced with `REDACTED`.

```bash
OPENAI_API_KEY=... DN_RECORD_FIXTURES=1 cargo test -p dn-providers --test live -- --ignored
```

Review the recorded file by hand before committing it. gitleaks runs in CI, but don't rely
on it.
