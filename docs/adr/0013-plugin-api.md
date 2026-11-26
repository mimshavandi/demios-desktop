# ADR-0013: Plugin API (`dn-plugin-api`): what add-ons may register, and how they stay inside the app's rules

**Status:** Accepted (owner, 2026-11-26), amendment proposed (§9, owner approval needed) · **Date:** 2026-11-25 (amended 2026-11-25 for [ADR-0014](0014-demios-node.md); amended 2026-11-26: missed-run rules; amendment proposed 2026-11-26: §9) · **Deciders:** project owner, lead · **Extends:** [ADR-0001](0001-repo-and-crate-layout.md), [ADR-0005](0005-agent-skill-manifests.md), [ADR-0006](0006-tools-mcp-capabilities.md), [ADR-0007](0007-local-storage-and-secrets.md), [ADR-0009](0009-security-baseline.md)

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

### 9. Amendment proposed (2026-11-26, owner approval needed): per-tool limits, withdrawing held calls, add-on views, and tools that always need a fresh approval
> **Status of this section: proposed.** §1–§8 stay as accepted. Where this section changes them, it says so, and it applies only once the owner approves it.

Why: an add-on that reads mail and drafts replies showed four gaps, each of which would otherwise be handled by a review rule or not at all.
1. Holding any write tool raises **every** tool of the add-on to `external_write`, so even "show today's summary" asks every time (§2, risk floor).
2. An add-on can't take back a call it put in the held queue, so an edited draft would leave the old version waiting for approval.
3. An add-on can show only a settings form (§2), so a summary, a list of items or a draft to review can't be shown on the desktop or a phone.
4. "Never send without the user's approval" depends on the user's per-tool choices. One "run without asking" choice, made by mistake, removes it.

#### 9.1 (a) Per-tool limits: every handler and job gets a `Host` limited to its own tier
**Changes §2 ("The risk floor") and §4.** Today the host limits only `read`-tier handlers. Instead, it limits every tier, and the add-on-wide floor goes away.

| Declared tier | Own data (memory, settings, files, database) | `http()` | `tools()` |
|---|---|---|---|
| `read` | read only | **none** | tools up to `read` |
| `low_write` | read and write | **none** | tools up to `low_write` |
| `external_write` and above | read and write | listed hosts (`network`); from a job, anything but GET is held, as in §2 | tools up to the handler's tier |

- **Enforced by the host,** through `Access` and `Caller::max_tool_tier` in `dn-plugin-api`, which already carry the tier.
  - `Access` gains a variant for `low_write`, so that `ReadWrite` means `external_write` and above.
  - `http()` is refused below `external_write`. That includes GET, since a GET can carry data out in its address.
  - In practice this changes nothing for the `read` tier. Today an add-on holding `network` has a floor of `external_write`, so no `read`-tier handler could call `http()`.
- **The floor goes away.** A tool's tier is what it declares, because its `Host` can't do more than that tier allows.
  - `dn_plugin_api::risk_floor` stays the one place the rule lives, and the `Registrar` stays its only caller. It changes from "the floor of the add-on" to "the lowest tier a tool may declare", which is `read` for every tool under this rule.
  - The function is kept rather than removed, so a later permission that can't be limited per handler has one place to put a floor back.
- **Jobs declare a tier too.**
  - `JobDecl` gains `tier`, and the job's `Host` is limited by it like a tool's.
  - An API minor version can only add, so a job declared without a tier keeps today's behaviour: `ReadWrite` with tools up to `sensitive`. The add-on build CI warns about it.
- **What this buys:** an add-on can split its work into handlers with separate powers, and the host enforces the split.
  - A handler that reads outside content runs at `read`, with no write tool and no `http()`.
  - A handler that writes drafts into its own database runs at `low_write`, with no tool that acts outside the app.
  - A handler that turns a stored draft into a request to send runs at `external_write`, and that request still needs approval (§9.4).
  - Outside text can then change, at most, the content of something that waits for the user.
- **What stays a review rule:** as in §1, an in-process add-on could reach past `Host` with raw `std` I/O. The CI checks (§1) and the conformance kit's new tier-limit scenarios are the backstops.

#### 9.2 (b) An add-on can withdraw or replace its own held call
**Adds to §2 ("Jobs can't do what needs a person") and §4.** `Host` gains `held()`, covering only calls this add-on put in the queue (enforced: the queue records the requester, and the host checks it).
- `list()`: its calls that are still waiting, with their ids, tools and arguments.
- `withdraw(id)`: the call becomes **withdrawn**, never runs, and its history says "withdrawn by *add-on*".
- `replace(id, new arguments)`: in one transaction, it withdraws the old call and holds a new one for the same tool, linked to the old as "replaces". Every device shows only the new one, and the review shows what changed (a diff of the arguments).
- **Only while held.** Withdrawing or replacing a call that has already been approved, is running or has finished fails with a clear error. The step from held to answered happens once (dn-jobs' guarded update), so an answer and a withdrawal can't both win.
- **An answer to a withdrawn call is refused**, and the device that gave it says "This was replaced by a newer version".
- Delete all and pause-all work as before. Withdrawn calls go into the run history only; they never go back into the queue.

#### 9.3 (c) Declarative add-on views, drawn by the host on desktop and phone
**Changes §2 ("Settings panels are declarative").** Settings panels stay as they are. An add-on can also register **views**.
- **A view** is an id, a title, where it appears (its own entry in the sidebar, or a section of Settings › Add-ons), and a **`read`-tier render handler**.
  - The handler returns a **view document**: a tree of blocks from a fixed set, as plain, serialisable data (§1).
  - **Blocks:** heading, text, notice (information or warning), key and value list, item list (each item has a title, a subtitle, a reason, badges and actions), draft (labelled fields, a body, and a diff against an earlier version), and empty state.
- **Every string is plain text.** There's no markup, HTML or script. Addresses are shown as text and open only through the app's link confirmation (the system dialog that shows the full address). The host draws every block with the design system's components.
- **Actions** name one of the add-on's own tools (`plugin.<id>.<name>`) and its arguments, or another of its views.
  - An action runs through the executor like any tool call, at the tool's tier, with the user's approval choices. A view can't run anything the user couldn't run from a chat.
  - Its approval scope is per view (`plugin:<id>:view:<view>`) and never shared with chats.
- **On a phone or another paired device,** the device asks the runtime (the desktop app or the Node) to render the view. The view document comes back over the paired channel, or sealed through the Relay. The device draws it with the same rules.
- **Limits** (enforced): at most 200 blocks and 4 levels of nesting, 2,000 characters per text block, 20,000 per draft body, and 10 actions per item.
- **Versions:** block kinds are `#[non_exhaustive]`. A host that meets a block kind it doesn't know shows "Update Demios to see this part" in its place, and never drops it silently.
- New views are designed in the design reference first (ADR-0010, design standard §8).

#### 9.4 (d) Tools that always need a fresh approval (`approval_required`)
**Adds to §2, ADR-0006 and ADR-0014 §6.** Some tools act for the user in a way that can't be taken back: sending a message, answering an invitation, paying.

**Which tools are in the class:**
- A tool is `approval_required` when:
  - its entry in the Connect catalogue marks it (ADR-0018 §1);
  - its add-on declares it so; or
  - the user marks it.
- A mark from the catalogue or an add-on **can't be removed** by a setting. A mark the user added can be removed by the user, after a confirmation.

**What the class changes** (enforced by the host):
- **No standing permission.** "Always allow in this chat", "run without asking" and the per-add-on `auto` choice aren't offered and aren't accepted. A stored choice that would allow one is ignored, and Settings shows that it is ignored.
- **A job's call is always held** (§2), whatever the settings.
- **The call runs only with a one-time approval token:**
  - it's bound to the held call's id, the tool, and the SHA-256 of the exact stored arguments, which is dn-jobs' signed answer message (`dn-held-answer v1`);
  - it's consumed by the held-to-approved step, which happens once.
- **The executor checks the token** immediately before it runs the call, and refuses to run an `approval_required` tool without one. It doesn't matter where the call came from: a chat, a view action or a job.
- **The token is minted only by a person's answer:**
  - in the app, by the approval card's **Allow once** (an app command that only the app's own page can call, #85); or
  - on another device, by an answer signed with that device's key and checked by the Node's `AnswerVerifier` against the device group (ADR-0014 §6).
- **No add-on, job, model output or tool can create a token.** An add-on's `Host` has no way to, and the token types aren't in `dn-plugin-api`.

**Two optional strengthenings for devices that can do them:**
- **Presence-bound keys.** A device whose signing key requires user presence for each signature (Face ID or Touch ID, Windows Hello) registers that at pairing. The user confirms it on the Node's pairing screen. For `approval_required` calls, the Node can then accept answers only from such devices. It's a setting, and it's off when no paired device has such a key.
- **Freshness.** An answer must be signed after the call was held and before its expiry. A copied old answer can't approve a new call, even with the same arguments, because the held call id differs.

**Be honest about the limits:**
- On a single computer without a Node, the token protects against every code path except the app process itself. An in-process add-on is still trusted code (§1), and the token is a check in the executor, not a sandbox.
- What the class does guarantee is that **no setting, schedule or add-on request** can make such a tool run without a person answering that one call.

#### 9.5 Consequences of the amendment
- **`dn-plugin-api`** (API minor version):
  - `Access` for every tier; `JobDecl::tier`;
  - `Host::held()` with list, withdraw and replace;
  - view registration and the view-document types;
  - the `approval_required` declaration on tools;
  - `risk_floor` returns `read` (§9.1).
  - The conformance kit gains scenarios for each.
- **`dn-core`:** `CapabilitySpec` gains `approval_required`. That's a contract change, and this ADR is its reference.
- **`dn-tools`:**
  - the executor ignores standing choices for the class, and checks and consumes the token;
  - the approval card hides "Always allow" for it, as it already does for destructive and sensitive tools.
- **`dn-jobs`** (#252):
  - `withdrawn` and `replaced` states, linked to the call that replaced them;
  - answers to them are refused;
  - the token check before `take_approved` hands out a call in the class;
  - the requester is recorded, for `held()`'s ownership check.
- **The app and paired devices:** a view renderer for the block set, approval cards for held calls that show diffs, and "replaced by a newer version".
- **Connect catalogue** (ADR-0018): entries gain the `approval_required` mark for send, answer and pay tools.

#### 9.6 Options considered for the amendment
| Gap | Chosen | Rejected | Why |
|---|---|---|---|
| Risk floor | Tier-limited `Host` for every tier | Keep the add-on-wide floor; ask add-ons to split into several add-ons with separate permissions | Splitting doubles consent screens and still leaves the split unenforced inside each add-on |
| Edited held calls | `withdraw` and `replace`, held state only | The user edits the arguments in the approval card | The add-on keeps its own copy of the draft; editing in two places drifts. Editing in the card may come later as a host feature |
| Add-on screens | Declarative blocks drawn by the host | Add-on HTML in a sandboxed frame like artifacts; native UI per platform | HTML would need a second capability model and couldn't be drawn on a phone without a webview; blocks keep text plain and look native everywhere |
| Never-without-approval tools | A tool class with one-time tokens checked by the executor | Rely on the user never choosing "run without asking"; a per-add-on switch | A setting can be changed by mistake; a class the catalogue or add-on sets can't |

#### 9.7 Action items (to be filed as issues once approved)
1. `dn-plugin-api`: per-tier `Access`, `JobDecl::tier`, the new `risk_floor`, and the conformance scenarios.
2. `dn-plugin-api` and `dn-jobs` (#252): `Host::held()`, the withdrawn and replaced states, and requester ownership.
3. `dn-plugin-api`: views and the view document. App: the renderer, designed in the design reference first.
4. `dn-core`, `dn-tools` and `dn-jobs`: the `approval_required` class, token checks, and Allow-once-only cards.
5. Pairing (ADR-0014 §2): the presence-bound key flag, and the Node setting that accepts only such devices for the class.
6. Connect catalogue: the class marks on send, answer and pay tools.

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
