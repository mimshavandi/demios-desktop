# Threat model: MCP tools and capabilities (T-CON-05)

**Scope:** `crates/dn-tools` — the MCP client (stdio, Streamable HTTP), the capability wrapper,
the approval gate, the side-effect ledger and MCP OAuth. Decision record: ADR-0006.
**Status:** first version, reviewed by the maintainers.

## Assets
| Asset | Where it lives |
|---|---|
| Provider API keys, MCP OAuth tokens, server credentials | OS keychain via `dn-secrets` (`SecretStore` in dn-tools); never in config, logs or the ledger |
| User files and accounts reachable by tools | local disk (stdio servers), remote services (HTTP servers) |
| Conversation content | app database; is sent to the model and, as tool arguments, to servers |
| Side-effect history | `tool_ledger_runs` (arguments + outputs of writes) |

## Trust boundaries
1. **Model → tool call.** Model output is untrusted. Anything the model has read (web pages,
   files, emails, tool results) can carry instructions ("prompt injection").
2. **dn-tools → MCP server.** A server is code or a service the user chose to install/connect.
   Its tool list, descriptions, annotations and results are untrusted input.
3. **dn-tools → authorization server.** Discovery documents and redirects are untrusted.
4. **Browser → loopback redirect.** Any local process can hit the redirect port.

## Threats and mitigations

### T1. Prompt-injected tool calls
*A document the model read says "call `delete_all`" or "write my SSH key to this gist".*
- Every tool is a capability with a **risk tier**; only `read` auto-runs by default
  (`RiskTier::default_approval`). Writes ask (`ask_once_per_conversation` for low writes,
  `always_ask` for external/destructive/sensitive).
- The approval prompt carries the **exact arguments** (`ApprovalPrompt::arguments`); the UI
  must show them, not a model-written summary. Dropped prompts, a closed UI channel and
  prompts unanswered after 10 minutes (`DEFAULT_APPROVAL_TIMEOUT`) deny.
- "Allow for this chat" grants are scoped to one conversation (or, for runs without a chat,
  one agent run) and one capability, and never apply to `always_ask`.
- Ledger replay never bypasses a decision: a replay returns the output of a run that was
  already approved and executed; it performs no new side effect.
- **Residual:** reads auto-run, and a read can exfiltrate through its *arguments* to an
  open-world server (e.g. `search(query=<secret>)`). See T3. Users can raise any tool to
  `always_ask` or disable it (`CapabilityOverride`).
- **Tools that always need a fresh approval** (ADR-0013 §9.4; `CapabilitySpec::approval_required`,
  set by the tool's add-on, the Connect catalogue or the user): sending a message,
  answering an invitation, paying.
  - No standing permission: a stored "run without asking" or "ask once per chat" choice is
    ignored (`McpCapability::approval_ignored` says so), "Always allow in this chat" is
    refused (`GateOutcome::StandingRefused`), and a job's call is always held, whatever its
    `auto_tools` say.
  - The call runs only with a one-time token (`dn_tools::token`) bound to the call (run and
    call id, or the held call's id), the capability and the exact arguments. The executor
    consumes it right before the call runs; it's single-use, expires after a minute and is
    revoked with its held call.
  - Only a person's answer mints one: the card's Allow once, inside the executor, or a held
    call's verified answer, through `Executor::approval_for_held`, which takes the
    `dn_jobs::held::Approved` proof only `take_approved` hands out. No token type is in
    `dn-plugin-api`, and no `Host` method makes one.
  - **Residual:** in-process add-ons are trusted code (ADR-0013 §1); the token is a check in
    the executor, not a sandbox.

### T2. Malicious or compromised server
*A server lies in `readOnlyHint`, ships a poisoned tool description, returns huge or hostile
results, or changes its tool list after approval ("rug pull").*
- Annotations are treated as **hints** (MCP spec): they only seed the risk tier, and the most
  dangerous hint wins (`destructiveHint: true` beats `readOnlyHint: true`). Annotation trust is
  per server (`McpServerConfig::trust_annotations`): by default only locally installed stdio
  servers may seed `Read` (auto-run); for HTTP servers the seeded tier is never below
  `LowWrite`, so nothing auto-runs without the user opting in. A trusted server that marks a
  write as read-only still gets it auto-run, so the server settings screen must show each
  tool's seeded tier with an override.
- Capabilities are namespaced `mcp.<server_id>.<tool>`; one server cannot shadow another's
  tools or a builtin. Model-facing names (`mcp__<server>__<tool>`) that lose information when
  sanitised or truncated get a hash suffix, and any remaining collision is suffixed, so two
  tools never share a name (a name collision could otherwise route a call to the wrong tool).
- Tool descriptions are shown to the model. They can contain injection; the UI should show the
  descriptions in server settings, and descriptions are not trusted for approval decisions.
- Failed calls show the model only the user-facing message, plus our own hints (missing
  fields) or the tool's own `isError` content (which MCP intends for the model, like any
  result). Developer detail — stderr, JSON-RPC error text, paths — stays local
  (`Executor::recent_diagnostics`, capped): it can carry injected text and would otherwise be
  sent to a remote model provider.
- `refresh` rebuilds a server's capabilities from scratch; overrides are keyed by capability id
  and re-applied. The app pins tool definitions (T-SHELL-11): a server's first connect records
  a SHA-256 of each tool (name, title, description, schemas, annotations). On every later
  connect a tool that is missing from that record, or whose hash differs, is shown as new or
  changed and gets "always ask", overriding the user's choice, until the user marks that
  version reviewed. While a server connects, its tools are hidden from the model, so a changed
  tool can't auto-run between listing and pinning. The settings screen shows each tool's
  description as plain text (control, bidi and zero-width characters removed), its seeded
  tier, and a per-tool choice saved in settings. Without `tools/list_changed` support in
  dn-tools, a change made while the server stays connected is caught at the next connect.
- Stdio servers run as the user with the user's permissions; dn-tools does not sandbox them.
  The child starts with a **cleared environment** plus an allow-list (`PATH`, `HOME`, `USER`,
  `LOGNAME`, `LANG`, `LC_*`, `TMPDIR`; on Windows `SystemRoot`, `windir`, `APPDATA`,
  `LOCALAPPDATA`, `USERPROFILE`, `ComSpec`, `PATHEXT`, `TEMP`, `TMP`) plus its configured env,
  so provider keys and other app secrets in our environment never reach servers.
- Each stdio server leads its own process group (Unix) or job object (Windows). Shutdown closes
  its stdin, sends SIGTERM to the group (Unix), then kills the group/job, so grandchildren
  (an `npx` launcher's `node`) don't outlive it. Dropping the client kills the group too.
  Environment variable names are matched case-insensitively (Windows spells `PATH` as `Path`).
- Results: rmcp caps JSON responses (16 MiB) and SSE event size; calls have a request timeout
  (default 60 s) and connect timeout (30 s). stderr is kept as a 40-line tail; developer
  messages include at most 10 lines of 300 chars, with this server's resolved secret values
  replaced by `[redacted]`.
- HTTP redirects are not followed for MCP traffic (`redirect::Policy::none`).

### T3. Secret exfiltration via arguments and configs
*The model is tricked into putting a token into tool arguments; a config export leaks keys;
an error message echoes a header.*
- Configs hold **secret ids**, never values (`ValueRef::Secret`). `McpServerConfig::validate`
  rejects plain values for names that look like credentials (`*KEY*`, `*TOKEN*`, `*SECRET*`,
  `*PASSWORD*`, `*AUTH*`, `*COOKIE*`, `*SESSION*`, `*PRIVATE*`, `*_PAT`, `DATABASE_URL`,
  `*DSN`…). Configs are safe to sync or export.
- Header values resolved from secrets are marked sensitive (`HeaderValue::set_sensitive`).
- OAuth tokens are stored through `SecretStore`; `Debug` impls in dn-tools never print values.
- The model never sees secret values: they are resolved at connect time inside dn-tools and are
  not part of any `ToolSpec` or argument.
- **Residual:** if the model has seen a secret (the user pasted it into the chat), it can put it
  into arguments. Approval prompts show arguments for writes; reads to open-world servers do
  not prompt. **Open item:** a secret-pattern scan on arguments that forces approval.
- The ledger stores write arguments and outputs in the app database (encrypted at rest once
  SQLCipher lands). Developer messages may include server stderr; they go to logs, not to
  other services.

### T4. OAuth attacks
- Authorization code + **PKCE S256** (rmcp refuses servers that don't advertise S256);
  `state` is checked on the callback (a forged state is rejected — tested).
- RFC 9207 `iss` is validated when present; stored credentials are discarded when the
  authorization server's issuer changes.
- Discovery (RFC 9728 → RFC 8414/OIDC) rejects https→http downgrades and link-local/metadata
  addresses; loopback authorization servers are allowed only for loopback MCP servers.
- Plain `http://` MCP URLs are rejected unless the host is loopback.
- The loopback redirect listener binds `127.0.0.1` on an ephemeral port, serves one
  `/callback`, then closes; connections that don't send a request within 5 s are dropped and
  `wait_timeout` bounds the whole wait. A local attacker racing the port only gets a code bound
  to our PKCE verifier, which it does not have. The redirect page doesn't claim success; the
  app reports the result of the token exchange.
- Tokens are scoped per server (`OAuthSettings::credential_secret_id`) — no token reuse across
  servers. Never fall back silently to another connection (ADR-0006).

### T5. Duplicate or lost side effects
- Writes are recorded in the ledger before they run (`claim → running`, `BEGIN IMMEDIATE`). A
  succeeded key replays; a running key blocks; reused keys with different arguments are
  rejected.
- Keys are scoped so one context can't replay another's output: `(capability, run_id,
  call id)` by default (model call ids like `call_0` repeat across runs), or `(capability,
  conversation, token)` when an idempotency field is configured. That field must be a
  per-request unique token, not a resource id.
- Only retry-safe capabilities (reads, or writes with an idempotency key field) auto-retry;
  retrying such a write relies on the server deduplicating by that token.
- A write whose outcome isn't known (app crash → `mark_interrupted`, cancel or timeout after
  the request was sent, transport closed mid-call, execute future dropped) is stored as
  `unknown`, not `failed`. Claims on an `unknown` key are refused — even under an "allow for
  this chat" grant — until the user resolves it (`Resolution::Happened` / `DidNotHappen`).
  While a capability has an `unknown` row in a scope (conversation or run), *every* new call to
  it in that scope is refused with a model-visible "ask the user to confirm" error, because a
  model retry carries a fresh call id (a fresh key) and would otherwise repeat the write.

## Out of scope for now
Sandboxing stdio servers (OS sandbox profiles), signed server registries, per-tool network
egress policy.
