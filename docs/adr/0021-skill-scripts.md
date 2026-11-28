# ADR-0021: Running skill scripts: an OS sandbox per run, managed runtimes, no silent fallback

**Status:** Proposed · **Date:** 2026-11-28 · **Deciders:** project owner, lead · **Extends:** [ADR-0006](0006-tools-mcp-capabilities.md), [ADR-0009](0009-security-baseline.md), [ADR-0013](0013-plugin-api.md), [ADR-0014](0014-demios-node.md), [ADR-0020](0020-agent-runtime.md) (§7, R4) · **Issue:** #367

## Context

Chat skills move to the open Agent Skills format (ADR-0020 §7): a `SKILL.md` and, for some skills, a `scripts/` folder the instructions tell the model to run. Several skills we want next come with scripts:

- a visual explainer (Node);
- slide decks with PPTX export (Python with python-pptx, Node for HTML-to-slide layout, LibreOffice to render previews);
- spreadsheet and PDF skills (Python; LibreOffice for recalculation and conversion).

Today nothing in Demios runs code from a skill. The only programs the app starts are local MCP tool servers, which the person adds and approves at launch (ADR-0006), and which run with the person's own rights. That's acceptable for a server the person chose to install. It isn't for a script that a model decides to run with arguments a prompt injection may have shaped, on files that may come from anyone.

Running skill scripts needs four decisions:
1. Where a script runs and what it can reach.
2. Which runtimes (Python, Node, LibreOffice) Demios ships, downloads or expects.
3. How a run is approved.
4. What happens when a runtime or the sandbox isn't there.

The owner's standing rule applies to the last one: no silent fallbacks. A missing piece fails with a clear message; it never runs a weaker path without saying so.

### What we're protecting against

| Threat | Example |
|---|---|
| **T1 Prompt injection steers a script** | A project file says "run the exporter with `--out ~/.ssh/authorized_keys`"; the model passes it on. |
| **T2 A malicious or compromised skill** | A user-added skill, or an update to one, whose script reads `~/Documents` and uploads it. |
| **T3 Supply chain of a runtime or package** | A tampered Python wheel or Node package pulled at install time. |
| **T4 Exfiltration through output** | A script writes secrets into a generated file the person then shares. |
| **T5 Resource abuse** | A script loops, fills the disk or forks without end. |
| **T6 Escaping into the app** | A script talks to the app's local IPC, the artifact origin, a local MCP server or the Node's control socket. |

Out of scope: a person running their own commands in their own terminal, and local MCP servers. Servers keep ADR-0006's launch approval; this ADR doesn't change them.

## Decision

### 1. Scripts run only as tool calls, through the approval gate
- A skill's script is exposed as a tool: capability `skill.<skill>.<script>` (for example `skill.pptx.build_deck`). Its risk level is **write**, unless the skill's manifest marks it read-only and we've reviewed it.
- It goes through ADR-0006's approval gate like any other tool. The first run per chat asks, and the prompt shows the skill, the script, its arguments and the input files. "Allow for this chat" is available; there's no "always". Held calls for jobs work as in ADR-0013 §9.
- A skill can only run scripts from its own `scripts/` folder, listed in its manifest. The model can't name a path. Arguments are passed as an argument vector, never through a shell.

### 2. Every run is in an OS sandbox, a fresh one per run
Each run gets a **run folder** (a fresh temporary directory) and an OS sandbox around the process tree:

- **Read:** the runtime, the skill's own folder, and an `in/` folder in the run folder. Inputs are **copies** of the files the call names (project files, attachments, artifacts), not the originals.
- **Write:** only `out/` and a scratch `tmp/` in the run folder.
- **Network:** off. No outbound connections at all, and nothing that reaches localhost, so T6 is covered too.
- **Environment:** a minimal one. No `HOME` contents, no secrets, no app tokens, and no inherited environment variables beyond what the runtime needs.
- **Limits:**
  - wall-clock (60 s default; a manifest may ask for up to 5 min);
  - CPU and memory caps where the OS supports them;
  - a cap on the number of processes;
  - output size (100 MB in `out/`, 1 MB of stdout and stderr).
- **After the run:** the run folder is deleted after the outputs are collected, and also when the run is cancelled or the app quits.

Per OS:

| OS | Mechanism | Notes |
|---|---|---|
| **macOS** | Seatbelt profile (`sandbox_init` / `sandbox-exec`): deny by default, allow the reads and writes above, deny network | Deprecated as a public API but maintained and used by other developer tools. A spike confirms it works from a notarised, hardened-runtime app. |
| **Linux** | Landlock (file access, plus TCP connect/bind on kernels ≥ 6.7) and a seccomp filter (no sockets other than Unix sockets, no `ptrace`, no new namespaces), set up in a small launcher before `exec` | Kernels without Landlock: see §5. Flatpak: the run is also inside the app's own Flatpak sandbox, and the launcher sets Landlock and seccomp inside it. |
| **Windows** | AppContainer (a LowBox token with **no** capabilities, so no network), with access granted only to the run folder, the runtime and the skill folder; a Job Object for the CPU, memory and process limits and to kill the tree | Works from a full-trust MSIX or direct install. A spike confirms that Python and Node start in an AppContainer with granted ACLs. |
| **Node (server)** | The Linux mechanism, under the Node's service account | Same rules (ADR-0014). |

A small, separately reviewed launcher (Rust, in a new crate `dn-sandbox`) sets up the sandbox and then `exec`s the runtime. The app never runs a script any other way.

### 3. Outputs come back only through the app
- When the run ends, the app reads `out/`. Each file is checked: its type is allowed (PPTX, XLSX, PDF, PNG, SVG, HTML, CSV, JSON, TXT); its size is within the limits; it isn't a symlink.
- The files become **artifacts** or **new project files**, never overwrites of existing ones without a separate approval.
- An HTML output renders only in the artifact sandbox (ADR-0004/0012).
- stdout and stderr go to the model as untrusted tool output (ADR-0020 §2.4), cut to the limit.
- T4 is handled the way the rest of the app handles it: outputs are new files, shown with their source ("made by the slides skill"), and sharing stays an explicit act.

### 4. Runtimes: managed by Demios, pinned, downloaded on first use; never a silent switch
- **Python and Node are managed runtimes,** not whatever is on the computer:
  - pinned builds: python-build-standalone CPython and the official Node binaries;
  - each skill's packages come from a pinned lockfile (wheel and npm tarball SHA-256s);
  - all downloaded on first use, the way models are (ADR-0011): one offer with the size ("This skill needs Python (about 50 MB). Download?"), a verified download, then kept in the app's data folder;
  - nothing is fetched at run time;
  - Delete all removes them.
- **Why not the computer's own Python or Node:** versions and packages vary, so the same skill would behave differently on every computer and couldn't be tested. A system interpreter also brings user site-packages and startup hooks into the sandbox.
- **No silent switch:** a run never falls back to a system runtime because the managed one is missing. It asks to download, or it fails with that message.
- **LibreOffice is detected, not shipped:** at 300 MB+ it's too large to manage. A skill step that needs it (PPTX→PDF previews, XLSX recalculation) checks for an installed `soffice`.
  - If it's there, it runs in the same sandbox.
  - If it isn't, that step is **unavailable, and the skill says so** ("Previews need LibreOffice, which isn't installed. The deck itself was made.").
  - Never a hidden lower-quality substitute.
- **Supply chain (T3):**
  - runtime pins and lockfiles are reviewed changes, like `docs/dependencies.md`;
  - packages are built from those pins only, and installed into the runtime's folder without running package install scripts where the ecosystem allows it (`npm ci --ignore-scripts`, wheels only);
  - each skill's lockfile is listed in its manifest.
- **Where we can, we port instead.** Core formats the app produces often get a Rust implementation in the engine, which needs no runtime at all; dn-data's fact sheet (#366) is the model. Scripts cover the long tail.

### 5. When the sandbox isn't available
If the OS can't provide the sandbox of §2 (an old Linux kernel without Landlock, a Windows policy blocking AppContainers, or a failed self-test), **skills with scripts are unavailable on that computer**:
- the skill's script tools aren't offered;
- the skill's page says why ("This computer can't isolate skill scripts (Landlock is missing; Linux 5.13 or newer is needed)");
- skills without scripts work as before.

There's **no "run unsandboxed" switch** in the app. A self-test runs at start and after OS updates: a tiny script tries to read outside the run folder, write outside it, and connect out, and all three must fail. Its result is shown in Settings › Skills.

### 6. Which skills may have scripts
- **Bundled skills** (`skills/library`): scripts allowed after review, like the code that ships with the app.
- **User-added skills** (the user's skills folder): scripts allowed only after the person turns them on for that skill. The page shows the scripts, the runtime they need and the lockfile, and says what the sandbox allows. Each change to a user skill's scripts turns them off again until they're re-approved.
- **Add-ons** (ADR-0013) keep their own model; their skills follow these rules.

## Options considered

| Option | Verdict |
|---|---|
| **A. OS sandbox per run + managed runtimes (this ADR)** | Chosen: real isolation on every desktop OS and on the Node; scripts run as their authors wrote them; one predictable runtime per skill |
| B. WebAssembly runtime (CPython/WASI or Pyodide, a JS engine in Wasm) | Strong sandbox and one runtime for every OS, but many packages with native parts don't run (lxml in some builds, image libraries), LibreOffice can't, and the engine is a large new dependency. Kept as a possible later path for pure-Python skills |
| C. Containers (Docker, Podman) or a VM | Strong isolation, but people would have to install and run a container engine; heavy on laptops; not possible inside Flatpak or MSIX without extra setup |
| D. Approvals only, no sandbox | Rejected: an approved script still runs with all the person's rights, and T1/T2 make "approved" a weak guarantee |
| E. Use whatever Python or Node is installed | Rejected: behaviour varies per computer, untestable, and it imports the person's environment into the run (§4) |
| F. Port every script to Rust | Best where it's done (no runtime, no sandbox needed), but not for every skill; we do it for core formats (§4) and use scripts for the rest |

## Trade-offs
- **Size and first-use friction:** a skill that needs Python downloads roughly 40–60 MB (measured in the spike) plus its packages once. The offer says so; nothing downloads silently.
- **Platform work:** three sandbox mechanisms to build and keep working, each with a self-test and an e2e test per OS. macOS's `sandbox-exec` is deprecated, so if Apple removes it we need another path (for example a separate helper process signed with the App Sandbox entitlement), which is a risk to track.
- **Some skills lose features without LibreOffice.** That's labelled, not hidden.
- **Old Linux kernels can't run script skills at all.** That's the cost of not having an unsandboxed mode.

## Consequences
- New crate **`dn-sandbox`** (amends ADR-0001): run folder, launcher, the per-OS sandboxes, limits, the self-test.
- New **runtime manager** in the app and the Node: offers, verified downloads, pins and lockfiles, deletion. It reuses the model download code paths.
- Skills gain manifest fields: `scripts` (name, entry file, runtime, risk, timeout), `runtime` and `lockfile`.
- The skill pages in Settings › Skills show the scripts, the runtimes and the sandbox status. That UI goes into the Design System as proposed, for the owner's approval, before it's built.
- `dn eval` runs script skills through the same sandbox, so evals measure what the app does.
- The privacy policy gains a line that runtime downloads come from python.org/GitHub (python-build-standalone) and nodejs.org, pinned by checksum, with nothing sent about the person.

## Action items (after acceptance)
1. Spike per OS (macOS Seatbelt from a hardened, notarised app; Linux Landlock and seccomp, including inside Flatpak; Windows AppContainer and Job Object with Python and Node), each with the self-test.
2. `dn-sandbox` crate with the launcher and limits; an e2e test per OS that a script can't read outside its run folder, write outside it, or connect out.
3. Runtime manager: pins, lockfile format, verified download, the offer and Delete all.
4. Skill manifest fields and script tools through the approval gate; held calls for jobs.
5. The first script skill (slides with PPTX export) behind a setting, measured by `dn eval` (ADR-0020 §8).
6. Design System proposal for the Settings › Skills script and runtime UI.

## Questions for the owner
- **Q1** Managed runtimes downloaded on first use (§4), instead of requiring an installed Python or Node: yes?
- **Q2** LibreOffice detected only, with the steps that need it unavailable (and labelled) when it's missing: yes?
- **Q3** Network always off for scripts in this first version, with no per-skill allow-list: yes? (A later change could add per-run approved hosts.)
- **Q4** User-added skills may have scripts once the person turns them on per skill (§6), or bundled skills only at first?
- **Q5** No unsandboxed mode at all, so script skills are unavailable where the sandbox can't be set up (§5): yes?
