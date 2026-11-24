# Provider HTTP fixtures

Response bodies the fixture server in `tests/common` replays to the real `reqwest`
client, so the adapters' HTTP, SSE and JSON handling run end to end with no network and
no keys.

- `*.sse`: streaming chat responses (`text/event-stream`), one file per scenario.
- `*.json`: non-streaming responses (`/models`, error bodies).

**Provenance.** These files are written to match each provider's documented streaming
wire format: real field names, event order and chunking (tool-call arguments split across
deltas, for example). Ids, signatures and key fragments are `REDACTED`. They are not raw
captures. To refresh one from a real response, capture it with `curl -N` against the API,
scrub ids and keys, and diff it against the old file. `tests/live.rs` (ignored; reads keys
from env vars) checks the adapters against the real APIs. Never commit a file that contains
a key: gitleaks runs in CI.
