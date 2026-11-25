# ADR-0013: Plugin API (`dn-plugin-api`): what add-ons may register, and how they stay inside the app's rules

**Status:** Proposed · **Date:** 2026-11-25 · **Deciders:** project owner, lead · **Extends:** [ADR-0001](0001-repo-and-crate-layout.md), [ADR-0005](0005-agent-skill-manifests.md), [ADR-0006](0006-tools-mcp-capabilities.md), [ADR-0007](0007-local-storage-and-secrets.md), [ADR-0009](0009-security-baseline.md)

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

## Decision

### 1. Isolation: in-process Rust crates, linked at build time, behind host services
- **An add-on is a Rust crate** that depends on `dn-plugin-api` (and nothing else from the app).
- **It is linked statically** into a separate build target, an add-on build, by a small registry list in that build's `main`.
- **No dynamic loading.** There is no `dlopen`: Rust has no stable ABI, and loading a library needs `unsafe`, which ADR-0009 forbids.
- **The open-source build links no add-ons** and contains no add-on code paths.
- **The add-on never gets the app's internals.** That rules out a `rusqlite::Connection` to other crates' tables, the `dn_tools::Executor`, the keychain or the webview. It gets a **`Host` handle** of narrow, permission-checked services (§4).
- **The API is shaped as declarations plus messages.** Every argument and result that crosses the boundary is a plain, serialisable value. So a later move to WASM (option B) changes how the boundary is called, not what it says.
- **Add-on crates must `#![forbid(unsafe_code)]`.** The add-on build's CI checks it, and checks `cargo deny` for the add-on's dependencies, like the app's own.

**Be honest about trust:** in-process means the add-on runs with the app's privileges. Its isolation is the API, code review and signed builds of a first-party, reviewed add-on, not a sandbox. That is why this path is for first-party add-ons only. **Third-party tools keep using MCP** (ADR-0006), which runs out of process and is covered by approvals. A third-party plugin system would need a real sandbox, which needs its own ADR (§8).

### 2. What an add-on may register
At start-up the host calls `Plugin::register(&mut Registrar)` once. Everything an add-on contributes is declared there. Nothing is added later behind the host's back.

```rust
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> &PluginManifest;          // id, version, api version, name, permissions
    fn register(&self, r: &mut Registrar) -> Result<(), PluginError>;
    fn migrations(&self) -> &'static [&'static str]; // its own tables only (§5)
    fn forget_conversation(&self, h: &Host, id: Id) -> Result<(), PluginError>;
    fn delete_all(&self, h: &Host) -> Result<(), PluginError>;
}
```

- **Tools** are `CapabilitySpec`s with a new source, `CapabilitySource::Plugin { plugin_id }`, and ids under `plugin.<plugin_id>.<name>`. A handler runs the call.
  - They go through **the same `dn_tools` path as MCP tools**: preflight, risk tier, the approval gate, the side-effect ledger with idempotency, the per-tool choices in Settings, and skill narrowing (`dn_tools::Narrowed`).
  - The add-on **declares** a risk. The host uses the **higher** of the declared risk and its own floor for the kind of action, so a plugin can't declare a write as `read`.
  - Approval comes from the risk tier and the user's choice, never from the plugin. There is no "auto-approve" field.
  - This adds a variant to a `dn-core` type, which is a contract change. This ADR is its reference.
- **Skills** are chat skills in the `SKILL.md` format (#117/#133), registered as values, with the same parser and the same rule that `allowed-tools` only narrows. A plugin skill can't take a built-in skill's name. Its source shows as the add-on's name.
- **Background jobs (heartbeat)** are declared with an id, a name, a schedule (an interval, or a local time of day) and a handler.
  - **They run only while the app runs.** They are scheduled by the host. There is no OS daemon, no launch agent, and no running after quit.
  - **They are off by default.** The user turns each one on in the add-on's settings and sees its last run, next run and last error. One switch pauses all background jobs.
  - The host honours the user's quiet hours and skips a missed run rather than catching up a burst.
  - **Jobs can't do what needs a person.** A job's tool calls go through the same approvals, and there is nobody to ask. So a call that would ask is **held** in a visible queue that waits for the user, never run, and never auto-approved. Jobs run reads, and tools the user explicitly set to run without asking.
  - A job gets a cancel token and a time budget. When either ends, the job is stopped.
  - A job reports to the user only through the host's notification service (§4), which follows the OS's notification permission.
- **Settings panels** are **declarative**: a JSON-Schema form with titles, help text and the design system's controls, rendered by the host UI.
  - An add-on ships **no HTML or JS** into the app's webview. That keeps the webview capability model (#85, ADR-0012) intact.
  - Values are stored by the host under `plugin.<id>.*`. Secret fields go to the keychain through `dn-secrets`, under ids the host prefixes with the add-on's id, so "Delete all" finds them (T-STORE-11).
- **Not registrable:** model backends or providers (those are `dn-core::chat::ChatModel` work in the app), webview commands, raw database or file-system access, or network access beyond what §4 grants.

### 3. Manifest and permissions
`PluginManifest` names the add-on (`id`: lowercase, dotted, unique), its version, the `dn-plugin-api` major it was built for, and the **permissions** it asks for:

| Permission | Grants | Scope |
|---|---|---|
| `chats.read` | read conversations and messages through `Host::chats()` | all chats, or the chats of chosen projects |
| `memory` | the add-on's own memory store (§5) | its own data only |
| `knowledge.search` | search the local index (`dn-knowledge`) | the projects the user chose |
| `notify` | OS notifications | — |
| `network` | outbound HTTPS, through the host client | listed hosts only |
| `jobs` | register background jobs | each job still off until the user turns it on |

- **The user consents per permission**, on the add-on's first start and again when a new version asks for more, in plain words naming what each one reads or does.
- Each permission can be revoked in Settings. A revoked permission makes its host calls fail with a clear error. They never return empty data.
- **The host checks the permission on every call,** not only once at registration.

### 4. Host services
`Host` is the only way an add-on reaches anything. Every method is permission-checked, and its results are copies:
- **`chats()`**: list and read messages in the consented scope. Returned text is tagged as **untrusted user or model content**. When an add-on puts it into a prompt, it must use the same quoted, cited block as `dn_knowledge::context_block`. The API offers a helper, and the conformance kit (§7) checks it.
- **`memory()`**: see §5.
- **`knowledge()`**: search in the consented projects. It returns `Hit`s and context blocks.
- **`chat_model(slot)`**: run a prompt on the model the user chose for that slot, through the app's `ChatModel` path.
  - It goes through model resolution as usual, with the same "no silent fallback to another model" rule.
  - The add-on never sees provider keys.
- **`tools()`**: call registered tools (its own, MCP or built-in) **as a request through the executor**, with the same approvals. An add-on can't call a tool around the approval gate.
- **`notify()`**, **`settings()`**, **`secrets()`** (its own prefixed ids only), and **`http()`** (listed hosts only, through the app's client, honouring the user's proxy settings). In-process code could open its own sockets, so for first-party add-ons "network only through `http()`" is a review rule, which the add-on build's CI backs with a check that no HTTP client or socket crate is a direct dependency.
- **`files()`**: a directory under `<data dir>/plugins/<id>/` that the host owns and deletes.

### 5. Storage, memory and "Delete all"
- **Tables.** An add-on's tables follow the persistence rule (CLAUDE.md): its own `migrations()`, every table prefixed `plugin_<id>_`, run by `dn-store` in its own migration set. Its handlers get a connection limited to its own tables through `Host`. They never get a handle to other tables.
- **Memory is the add-on's own data.** The host gives a small store of items (text, source chat ids, created/updated), so the UI can show the user **what the add-on remembers**, with delete per item and delete all.
  - An add-on that keeps memory elsewhere (its own tables) must still expose those items through the same listing. The conformance kit checks that both agree.
- **Delete all local data never skips an add-on.** The host:
  1. calls `delete_all` on every add-on;
  2. **then** drops every `plugin_*` table and the `plugins/` directory itself, and deletes the add-on's prefixed keychain ids, whether or not the add-on's own step succeeded;
  3. lists any failure in the "couldn't delete" report, as it does for the app's own data.
- **Deleting a chat** calls `forget_conversation` when the chat is purged. The add-on must drop everything derived from it, including memory items whose only source was that chat. Missing or failing purges are reported.
- **No copies outside the host's places.** An in-process crate could technically write elsewhere, so this is a review rule for first-party add-ons, and the conformance kit checks that after `delete_all` nothing of the add-on is left in the data dir.

### 6. Licence keys: offline, signed, consented
- **The key is a signed licence file:** licensee, add-on ids, expiry and an issue id, signed with Ed25519. The add-on build carries the **public** key and verifies the file **locally**.
  - **No network check at start, and no phoning home.**
  - The key lives in the keychain.
- **An online check is optional and consented.** Refreshing, moving to another computer or revoking a key happens only when the user asks, from Settings. It sends the issue id and nothing about their data or usage. It is off by default and documented.
- **When a licence expires or is invalid,** the add-on's features stop with a clear message naming the reason, such as "Your licence ended on …". Other options were rejected:
  - **No silent degrading.**
  - **The user's data is never locked:** the add-on's memory stays viewable and deletable, and "Delete all" always works.
  - **Nothing in the open-source build changes.**
- **Licence logic lives in the add-on build**, not in `dn-plugin-api`. The API only defines `LicenceStatus`, which the host shows in Settings. Signature verification needs a dependency (for example `ed25519-dalek`), which is decided and recorded where it is used.

### 7. Versioning and stability
- **Major versions.** `dn-plugin-api` has its own semver and an `API_MAJOR` constant. A manifest names the major it was built for.
  - **A mismatch is refused with a clear error**: "This add-on needs Demios … ; update Demios or the add-on."
  - There is no compatibility shim and no partial load.
- **Minor versions** only add. Public structs and enums are `#[non_exhaustive]`, and new capabilities come as new methods with default implementations.
  - Removing or changing anything is a major version and needs an ADR.
  - A deprecated item keeps working for at least one minor version and warns in the add-on build.
- **Dependencies.** `dn-plugin-api` depends only on `dn-core` and `serde`/`serde_json`, and re-exports the types it uses, so add-ons don't pin other app crates.
- **Conformance kit.** A `testing` feature ships a host fake that runs an add-on through: registration, a permission revoked mid-run, `forget_conversation`, `delete_all` with leftovers checked, held approvals for jobs, and the untrusted-text helper.
- **Failure handling.** An add-on that panics or errors in `register` is **not loaded**. Settings shows why. A handler that panics fails that call with a clear error, and the host disables that add-on until restart. There is no silent retry loop.

### 8. Third-party plugins: not yet
Third-party, untrusted add-ons would need a real sandbox: WASM components or a separate process. That is option B or C below, with the same host-service API over a message boundary.

This ADR only keeps that path open (§1). It doesn't build it. Until then, third parties extend Demios through **MCP servers** and **skills**.

## Options considered

| Option | Isolation | Fit for heartbeat, settings and memory | Effort | New dependencies | Notes |
|---|---|---|---|---|---|
| **A. In-process Rust crate, static, behind host services (chosen, first-party)** | API, review and signed build; no sandbox | Full | Low | None | Matches ADR-0001's separate build target and git-tag dependency |
| B. WASM component (wasmtime, WIT) | Real sandbox: memory, CPU, no ambient I/O | Full, with a WIT host API | High: async host calls, component tooling, debugging | wasmtime (large) | The way for **third-party** plugins later; same API shape |
| C. Separate process over IPC or MCP | OS process boundary | Tools only through MCP today; jobs, settings and memory need a new protocol | Medium–high | Process management already exists (dn-tools stdio) | MCP covers third-party tools already |
| D. Dynamic library (`dlopen`) | None | Full | Medium | libloading | Rejected: needs `unsafe`, no stable Rust ABI, one compiler per build |

## Trade-offs
- **A trusts the add-on's code** as much as the app's own. That's acceptable only because add-ons are first-party, reviewed, built and signed with the app. The decision depends on that, and §8 keeps third parties out of it.
- **Declarative settings panels** limit what an add-on can show. That's deliberate: custom UI in the main webview would bypass the capability model. A richer panel would be a new host component, added through an API minor version.
- **Background jobs that hold, rather than ask,** make unattended work less capable. That's the point: an approval needs a person.
- **An offline licence** can't be revoked instantly. Expiry dates bound that, and users get privacy and offline use in exchange.

## Consequences
- `dn-core` gains `CapabilitySource::Plugin { plugin_id }`. dn-tools learns to run in-process handlers alongside MCP clients, through the same executor path.
- The app gains a plugin registry, a job scheduler with a visible held-approvals queue, a declarative settings-panel renderer, Settings › Add-ons (permissions, jobs, memory, licence status), and the "Delete all" and purge hooks.
- The open-source build is unchanged in behaviour. It registers no add-ons, and Settings › Add-ons doesn't appear.

## Action items (to be filed as issues)
1. `dn-plugin-api`: the traits and types above, plus the conformance kit (`testing` feature).
2. `dn-core` `CapabilitySource::Plugin`, and in-process tool handlers in the dn-tools executor (same approvals and ledger).
3. App: plugin registry, permissions and consent UI, Settings › Add-ons.
4. App: background job scheduler, quiet hours, held-approvals queue, pause-all.
5. App: declarative settings-panel renderer.
6. Storage: per-add-on migration sets, scoped connection, "Delete all" and purge hooks, and a leftover check.
7. Threat model for add-ons (extends ADR-0009): the in-process trust boundary, jobs, network allow-list.
