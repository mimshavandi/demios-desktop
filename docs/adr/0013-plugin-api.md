# ADR-0013: Plugin API (`dn-plugin-api`): what add-ons may register, and how they stay inside the app's rules

**Status:** Proposed · **Date:** 2026-11-25 (amended 2026-11-25 for [ADR-0014](0014-demios-node.md); amended 2026-11-26: missed-run rules) · **Deciders:** project owner, lead · **Extends:** [ADR-0001](0001-repo-and-crate-layout.md), [ADR-0005](0005-agent-skill-manifests.md), [ADR-0006](0006-tools-mcp-capabilities.md), [ADR-0007](0007-local-storage-and-secrets.md), [ADR-0009](0009-security-baseline.md)

## Context
ADR-0001 reserves `dn-plugin-api` as the "stable trait surface that add-ons plug into". Add-ons, including proprietary ones in a separate repository, build on it. ADR-0001 also sets two constraints:
- **A build with add-ons is a different build target.** It is not a runtime unlock that hides code in the open-source binary.
- **The private repository depends on these crates by git tag.**

The crate is an empty placeholder today.

An add-on needs more than MCP gives a third-party tool server:
- tools that run with the app's own data;
- skills;
- **work that runs on a schedule** (a heartbeat), for example a daily summary;
- a settings panel;
- read access to chats and to its own long-term memory.

Each of these is a way around the app's rules unless the API is designed against that. The rules are:
- risk tiers and approvals (ADR-0006, #84);
- skills that only narrow (#117);
- untrusted retrieved text (ADR-0007);
- "Delete all local data" removes everything;
- no telemetry (ADR-0009);
- no silent fallbacks.

This ADR decides the shape of the API. It doesn't implement it.

Every rule below says whether it is **enforced by the host** or is a **review rule** for first-party add-ons. It never claims enforcement it can't deliver.

## Decision

### 1. Isolation: in-process Rust crates, linked at build time, behind host services
- **An add-on is a Rust crate** that depends on `dn-plugin-api` (and nothing else from the app).
- **It is linked statically** into a separate build target, an add-on build, by a small registry list in that build's `main`.
- **No dynamic loading.** There is no `dlopen`: Rust has no stable ABI, and loading a library needs `unsafe`, which ADR-0009 forbids.
- **The open-source build links no add-ons and contains no add-on code.** It does contain the generic hooks: the `Plugin` capability source, the in-process handler path in the executor and an empty registry. These stay unused there, and Settings › Add-ons doesn't appear.
- **The add-on never gets the app's internals.** That rules out a database connection, the `dn_tools::Executor`, the keychain or the webview. It gets a **`Host` handle** of narrow, permission-checked services (§4).
- **`Host` runs in the desktop app or in a Demios Node** (ADR-0014), the same engine run headless as a service. An add-on build exists for both. The add-on sees the same `Host` API in either, and doesn't know or need to know which one it runs in. Where a service differs (a Node has no window, so `notify()` reaches the user through paired devices), the host says so in that service's documentation, not through a different API.
- **The API is shaped as declarations plus messages.** Every argument and result that crosses the boundary is a plain, serialisable value. So a later move to WASM (option B) changes how the boundary is called, not what it says.

**Be honest about trust:** in-process means the add-on runs with the app's privileges. Rust can't stop it from using `std::net`, `std::process` or `std::fs` itself. Its isolation is the `Host` API, code review and signed builds of a first-party, reviewed add-on, **not a sandbox**.

The add-on build's CI backs review with checks. They are **backstops, not guarantees**:
- `#![forbid(unsafe_code)]`;
- a `cargo deny` ban list of network, process and FFI crates, **including transitive dependencies**;
- a gate that fails on any use of `std::net`, `std::process` or `std::fs` in add-on crates, and of the same I/O re-exported by allowed crates (for example `tokio::net`, `tokio::fs` and `tokio::process`, so add-ons use `tokio` without those features).

That is why this path is for first-party add-ons only. **Third-party tools keep using MCP** (ADR-0006), which runs out of process and is covered by approvals. A third-party plugin system needs a real sandbox, which needs its own ADR (§8).

### 2. What an add-on may register
At start-up the host calls `Plugin::register(&mut Registrar)` once. Everything an add-on contributes is declared there. Nothing is added later behind the host's back.

```rust
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> &PluginManifest;          // id, version, api version, name, permissions
    fn register(&self, r: &mut Registrar) -> Result<(), PluginError>;
    fn migrations(&self) -> &'static [&'static str]; // run against its own database file (§5)
    fn forget_conversation(&self, h: &Host, id: Id) -> Result<(), PluginError>;
    fn delete_all(&self, h: &Host) -> Result<(), PluginError>;
}
```

- **Tools** are `CapabilitySpec`s with a new source, `CapabilitySource::Plugin { plugin_id }`, and ids `plugin.<plugin_id>.<name>`. A handler runs the call.
  - They go through **the same `dn_tools` path as MCP tools**: preflight, risk tier, the approval gate, the side-effect ledger with idempotency, the per-tool choices in Settings, and skill narrowing (`dn_tools::Narrowed`).
  - This adds a variant to a `dn-core` type, which is a contract change. This ADR is its reference.
  - **The risk floor is computed from the add-on's permissions**, because the host can't see what an opaque handler does. A plugin tool's tier is at least:
    - `external_write` if the add-on holds `network` or `tools.call` of any write tool;
    - `low_write` if it can write memory, settings, files or its database;
    - otherwise the tier it declares.

    The add-on can only declare a **higher** tier than its floor.
  - **A handler is limited to its declared tier** (enforced by the host). The `Host` handed to a `read`-tier handler is read-only: `http()` GET only, no memory, settings, file or database writes, and `tools()` limited to the `read` tier.
  - Approval comes from the tier and the user's choice, never from the plugin. There is no "auto-approve" field.
- **Skills** are chat skills in the `SKILL.md` format (#117/#133), registered as values, with the same parser and the same rule that `allowed-tools` only narrows. A plugin skill can't take a built-in skill's name. Its source shows as the add-on's name.
- **Background jobs (heartbeat)** are declared with an id, a name, a schedule (an interval, or a local time of day) and a handler.
  - **They run whenever a runtime is up:** the desktop app while it's open, or a Demios Node (ADR-0014), which runs as a service while its machine is on. They are scheduled by the host.
    - **One scheduler at a time.** When a Node is paired, the Node owns the schedule and the desktop app doesn't run jobs, so a job never runs twice. With no Node, the desktop app runs them while it's open.
    - The desktop app itself still installs no OS daemon or launch agent. Running after the app quits is what a Node is for, and the user installs one as a separate, explicit step.
  - **They are off by default.** The user turns each one on in the add-on's settings and sees its last run, next run and last error. One switch pauses all background jobs.
  - **Quiet hours.** The host honours the user's quiet hours. A run that falls inside them waits until they end.
  - **Missed runs.** When no runtime was up at a job's scheduled time (the app was closed, the Node's machine was off), the runtime works out what was missed when it starts again, from the job's schedule and its last successful run, both stored locally. Nothing is kept on a server for this, and no server runs jobs. Each job has a **missed-run rule**. The add-on proposes it in the job declaration, and the user can change it per job:
    - **`catch_up_once`** (default), with a `fresh_for` window that the add-on declares (for example 6 hours). If the latest missed time is still inside the window, the job runs **once** and is marked "late". If it isn't, the missed run is skipped. Example: a 07:00 brief runs late at 10:00, but not the next day at 07:05, when the next scheduled run is due anyway.
    - **`ask`**: the user gets one message per job ("*Weekly report* missed 2 runs while Demios was off. Run it now?") with **Run now** and **Skip**. Fits jobs that cost money or send something.
    - **`skip`**: missed runs are dropped. Fits work that's only useful on time, such as a reminder.
  - **Safeguards for missed runs.**
    - **Never a burst.** However many runs were missed, each job catches up at most once, and catch-ups start one at a time.
    - **The decision is a fixed rule, not a model's judgement.** The same schedule, last run and clock always give the same result, so it is predictable and testable, and it costs nothing.
    - **The job is told it's late.** Its context carries the scheduled time and the time of the last successful run, so its work can cover the whole gap, for example "since Tuesday 07:00".
    - **Late runs follow the same rules.** They wait for quiet hours to end, their tool calls are held as below, and the pause-all switch also stops catch-ups and pending "Run now?" questions.
    - **It's never silent.** Each job's status shows "missed at 07:00, ran late at 10:02", "skipped (not fresh)" or "waiting for you", and the run history keeps it.
  - **Jobs can't do what needs a person.**
    - A job's tool calls go through approvals as usual, but `auto` applies **only to tools the user explicitly set to "run without asking" for this add-on**. The `read`-tier default doesn't count.
    - Every other call is **held**. It goes into a persisted queue that waits for the user, shows the add-on, the job and the exact arguments, and is never auto-approved. It doesn't time out into a silent denial; it stays held until the user answers or dismisses it.
    - "Delete all" clears the queue.
    - **Every `http()` call from a job that isn't a GET is held** the same way (enforced). The host can't tell where an argument's text came from, so it doesn't try to.
  - A job gets a cancel token and a time budget. When either ends, the job is stopped.
  - A job reports to the user only through the host's notification service (§4), which follows the OS's notification permission. On a Node, held approvals and notifications reach the user on their paired devices (ADR-0014 §4, §6).
- **Settings panels** are **declarative**: a JSON-Schema form with titles, help text and the design system's controls, rendered by the host UI.
  - An add-on ships **no HTML or JS** into the app's webview. That keeps the webview capability model (#85, ADR-0012) intact.
  - Every string an add-on supplies is shown as plain text, never as markup. That covers titles, help text, notification text, approval text and memory items.
  - Values are stored by the host in the add-on's own settings namespace. Secret fields go to the keychain through `dn-secrets`, under ids the host owns for that add-on (§5), so "Delete all" finds them (T-STORE-11).
- **Not registrable:** model backends or providers (those are `dn-core::chat::ChatModel` work in the app), webview commands, raw database or file-system access, or network access beyond what §4 grants.

### 3. Manifest and permissions
`PluginManifest` names the add-on, its version, the `dn-plugin-api` major it was built for, and the **permissions** it asks for.

**Ids.** An id is dot-separated segments, each `[a-z][a-z0-9]*` (no `_` or `-`).
- **The host keeps a registry** of which add-on owns each tool, table file, settings key, secret id and directory, and checks ownership against it, **not by string prefix**.
- Two add-ons whose ids are equal, or where one is a prefix segment of the other (`foo` and `foo.bar`), are refused at load with a clear error.

| Permission | Grants | Scope |
|---|---|---|
| `chats.read` | read conversations and messages through `Host::chats()` | all chats, or the chats of chosen projects |
| `memory` | the add-on's own memory store (§5) | its own data only |
| `knowledge.search` | search the local index (`dn-knowledge`) | the projects the user chose |
| `tools.call` | call tools that aren't its own, through `Host::tools()` | the capability ids or prefixes listed in the manifest, shown in the consent screen; calls are still approved per the user's choices |
| `notify` | OS notifications | — |
| `network` | outbound HTTPS, through the host client | listed hosts only; the consent text says data sent there isn't covered by "Delete all" |
| `jobs` | register background jobs | each job still off until the user turns it on |

- **Without `tools.call`,** an add-on can call only its own `plugin.<id>.*` tools.
- **The user consents per permission**, on the add-on's first start and again when a new version asks for more, in plain words naming what each one reads or does.
- Each permission can be revoked in Settings. A revoked permission makes its host calls fail with a clear error. They never return empty data.
- **The host checks the permission on every call** (enforced), not only once at registration.

### 4. Host services
`Host` is the only way an add-on reaches anything. Every method is permission-checked (enforced), and its results are copies.

**Untrusted content.** Every result that carries content is typed as `Untrusted<T>`:
- chat messages;
- tool outputs;
- memory items;
- knowledge hits;
- `chat_model()` output built from any of them;
- `http()` responses.

`chat_model()` takes untrusted values **only** through the quoted, cited block helper, the same format as `dn_knowledge::context_block`. Its instruction parameters take plain `String`s, never `Untrusted<T>`, so the types keep untrusted text out of the instructions **of that call**.

To use the text otherwise, an add-on calls `Untrusted::into_inner()`, a named accessor that is easy to find in review. What it does with the text after that, including feeding it back as instructions or as tool or `http()` arguments, is a **review rule**. The types don't track it.

- **`chats()`**: list and read messages in the consented scope.
- **`memory()`**: see §5.
- **`knowledge()`**: search in the consented projects.
- **`chat_model(slot)`**: run a prompt on the model the user chose for that slot, through the app's `ChatModel` path.
  - It goes through model resolution as usual, with the same "no silent fallback to another model" rule.
  - The add-on never sees provider keys.
- **`tools()`**: request a tool call **through the executor**, so the approval gate always runs; an add-on can't call around it. For every add-on call **the host builds the call context** (enforced):
  - **Grants:** its approval scope is `plugin:<id>:<run>`, which **never shares** "allow for this chat" grants with a chat.
  - **Narrowing:** a call made inside a plugin tool handler **inherits the caller's narrowing** (the chat skill's `Narrowed` allowlist and the handler's tier limit) and can only narrow it further.
  - **Prompts:** every approval prompt from an add-on call **names the add-on** (and the job, if any), so it never looks like the model's own request. That adds a requester field to `ApprovalPrompt`.
- **`notify()`**, **`settings()`** and **`secrets()`** (its own ids only).
- **`http()`**: listed hosts only, through the app's client, honouring the user's proxy settings. Using `http()` rather than its own sockets is a review rule, backed by the CI checks in §1.
- **`files()`**: read, write, list and delete operations on the add-on's own area under `<data dir>/plugins/<id>/`, which the host owns and deletes. It hands out no path, so an add-on needs no file-system API of its own. Not touching files elsewhere is a review rule, backed by the same checks.

### 5. Storage, memory and "Delete all"
- **Each add-on gets its own SQLite database file**, `<data dir>/plugins/<id>/plugin.sqlite3` (enforced).
  - Its `migrations()` and every handler query run only against that file, through a connection the host opens.
  - **`ATTACH` and other schemas are refused** by a rusqlite authorizer, so neither a migration nor a query can reach the app database, other add-ons' files or the app's tables. That includes triggers on another schema.
  - A handler's write rights follow its tier (§2).
- **Memory is the add-on's own data.** The host gives a small store of items (text, source chat ids, created/updated), so the UI can show the user **what the add-on remembers**, with delete per item and delete all.
  - An add-on that also keeps derived data in its own database must expose it through the same listing. That is a review rule; the conformance kit (§7) checks that the two agree in its scenarios.
- **Delete all local data never skips an add-on.** The host:
  1. **first** pauses the scheduler, cancels running jobs and handlers, and revokes every `Host` handle, so nothing writes during or after the deletion;
  2. calls `delete_all` on every add-on, with a fresh `Host` that can only delete the add-on's own data;
  3. **then** deletes every add-on's database file and `plugins/` directory itself, its settings namespace, its keychain ids from the ownership registry, and the held-approvals queue, whether or not the add-on's own step succeeded;
  4. lists any failure in the "couldn't delete" report, as it does for the app's own data.
- **When a chat is purged:**
  - The host itself deletes memory items whose only source is that chat, and held approvals from that chat (enforced).
  - It then calls `forget_conversation` on each add-on, which must drop everything else derived from it.
  - If an add-on isn't loaded (version mismatch, failed `register`, disabled, licence ended), its purges are **queued** in the app database. The add-on can't be used again until they have run.
  - A failing purge is reported and retried before the add-on's next use.
- **Copies elsewhere.** Data sent to a listed host through `http()` is outside the app and can't be deleted by it. The `network` consent says so. Keeping no other copies is a review rule (§1).

### 6. Licence keys: offline, signed, consented
- **The key is a signed licence file:** licensee, add-on ids, expiry and an issue id, signed with Ed25519. The add-on build carries the **public** key and verifies the file **locally**.
  - **No network check at start, and no phoning home.**
  - The key lives in the keychain.
- **An online check is optional and consented.** Refreshing, moving to another computer or revoking a key happens only when the user asks, from Settings. It sends the issue id and nothing about their data or usage. It is off by default and documented.
- **When a licence expires or is invalid,** the add-on's features stop with a clear message naming the reason, such as "Your licence ended on …". Other options were rejected:
  - **No silent degrading.**
  - **The user's data is never locked:** the add-on's memory stays viewable and deletable, purges still run (§5), and "Delete all" always works.
  - **Nothing in the open-source build changes.**
- **Licence logic lives in the add-on build**, not in `dn-plugin-api`. The API only defines `LicenceStatus`, which the host shows in Settings. Signature verification needs a dependency (for example `ed25519-dalek`), which is decided and recorded where it is used.

### 7. Versioning and stability
- **Major versions.** `dn-plugin-api` has its own semver and an `API_MAJOR` constant. A manifest names the major it was built for.
  - **A mismatch is refused with a clear error**: "This add-on needs Demios … ; update Demios or the add-on."
  - There is no compatibility shim and no partial load. Queued purges still run (§5).
- **Minor versions** only add. Public structs and enums are `#[non_exhaustive]`, and new capabilities come as new methods with default implementations.
  - Removing or changing anything is a major version and needs an ADR.
  - A deprecated item keeps working for at least one minor version and warns in the add-on build.
- **Dependencies.** `dn-plugin-api` depends only on `dn-core` and `serde`/`serde_json`, and re-exports the types it uses, so add-ons don't pin other app crates.
- **Conformance kit.** A `testing` feature ships a host fake that runs an add-on through:
  - registration;
  - a permission revoked mid-run;
  - `forget_conversation`, and a queued purge;
  - `delete_all`, with leftovers checked;
  - held approvals for jobs;
  - the tier limits of a handler's `Host`.

  It shows the add-on behaves in those scenarios. It **can't prove** behaviour outside them, which stays a review rule.
- **Failure handling.** An add-on that panics or errors in `register` is **not loaded**. Settings shows why. A handler that panics fails that call with a clear error, and the host disables that add-on until restart. There is no silent retry loop.

### 8. Third-party plugins: not yet
Third-party, untrusted add-ons would need a real sandbox: WASM components or a separate process. That is option B or C below, with the same host-service API over a message boundary.

This ADR only keeps that path open (§1). It doesn't build it. Until then, third parties extend Demios through **MCP servers** and **skills**.

## Options considered

| Option | Isolation | Fit for heartbeat, settings and memory | Effort | New dependencies | Notes |
|---|---|---|---|---|---|
| **A. In-process Rust crate, static, behind host services (chosen, first-party)** | API (permissions, scoped call contexts, own database file), review and signed build; no sandbox | Full | Low–medium | None in the API crate | Matches ADR-0001's separate build target and git-tag dependency |
| B. WASM component (wasmtime, WIT) | Real sandbox: memory, CPU, no ambient I/O | Full, with a WIT host API | High: async host calls, component tooling, debugging | wasmtime (large) | The way for **third-party** plugins later; same API shape |
| C. Separate process over IPC or MCP | OS process boundary | Tools only through MCP today; jobs, settings and memory need a new protocol | Medium–high | Process management already exists (dn-tools stdio) | MCP covers third-party tools already |
| D. Dynamic library (`dlopen`) | None | Full | Medium | libloading | Rejected: needs `unsafe`, no stable Rust ABI, one compiler per build |

## Trade-offs
- **A trusts the add-on's code** as much as the app's own. That's acceptable only because add-ons are first-party, reviewed, built and signed with the app. The decision depends on that, and §8 keeps third parties out of it.
  - What A **does** enforce is everything that goes through `Host`:
    - permissions;
    - scoped call contexts and inherited narrowing;
    - handler tier limits;
    - the add-on's own database file with `ATTACH` refused;
    - ownership checks;
    - deletion.
  - What it can't enforce (raw std I/O) is listed as review rules with CI backstops.
- **Declarative settings panels** limit what an add-on can show. That's deliberate: custom UI in the main webview would bypass the capability model. A richer panel would be a new host component, added through an API minor version.
- **Background jobs that hold, rather than ask,** make unattended work less capable. That's the point: an approval needs a person.
- **A risk floor from permissions** makes some plugin tools ask more often than their author would like. Every add-on has its own database file, so in practice almost every plugin tool is at least `low_write` and asks once per chat by default. This fails safe. A tool that only reads its own data can avoid that by being registered by an add-on without `network`, or by asking for narrower permissions.
- **An offline licence** can't be revoked instantly. Expiry dates bound that, and users get privacy and offline use in exchange.

## Consequences
- **`dn-core`** gains:
  - `CapabilitySource::Plugin { plugin_id }`;
  - a requester field on approval prompts.
- **dn-tools** learns to run in-process handlers alongside MCP clients, through the same executor path, and accepts host-built call contexts with their own approval scope.
- **The app and the Node** (ADR-0014) gain:
  - a plugin registry with an ownership table;
  - a job scheduler with a persisted held-approvals queue, run by one runtime at a time;
  - a declarative settings-panel renderer;
  - Settings › Add-ons (permissions, jobs, memory, licence status);
  - per-add-on database files;
  - queued purges and the "Delete all" sequence above.
- **The open-source build** is unchanged in behaviour. It registers no add-ons, and Settings › Add-ons doesn't appear.

## Action items (to be filed as issues)
1. `dn-plugin-api`: the traits and types above, `Untrusted<T>`, and the conformance kit (`testing` feature).
2. `dn-core` `CapabilitySource::Plugin` and the approval requester field. In-process tool handlers in the dn-tools executor, with host-built call contexts (own scope, inherited narrowing, tier limits).
3. App: plugin registry and ownership table, permissions and consent UI, Settings › Add-ons.
4. App and Node: background job scheduler, quiet hours, missed-run rules (`catch_up_once` with `fresh_for`, `ask`, `skip`) with their status and history, a persisted held-approvals queue, pause-all.
5. App: declarative settings-panel renderer (plain text only).
6. Storage: per-add-on database files with an authorizer, queued purges, the "Delete all" sequence, and a leftover check.
7. Add-on build CI: a `cargo deny` ban list (transitive) and a gate on `std::net`, `std::process` and `std::fs`.
8. Threat model for add-ons (extends ADR-0009): the in-process trust boundary, jobs, network allow-list, prompt injection through untrusted results.
