# Documentation

Docs for people who use, build or contribute to Demios Desktop. Start with the [project README](../README.md) and [CONTRIBUTING.md](../CONTRIBUTING.md).

## Using the app

| Doc | What it covers |
|---|---|
| [user/](user/README.md) | User guide: getting started, adding a model, chats, tools and approvals, privacy, reporting a problem, shortcuts, troubleshooting, the `dn` tool |
| [../CHANGELOG.md](../CHANGELOG.md) | What changed in each version |

## Building and testing

| Doc | What it covers |
|---|---|
| [apps/desktop/README.md](../apps/desktop/README.md) | The desktop app: layout, dev commands, the test model, IPC rules |
| [apps/desktop/e2e/README.md](../apps/desktop/e2e/README.md) | Real-app end-to-end tests, the manual macOS release checklist and the unsigned macOS preview build |
| [dependencies.md](dependencies.md) | Every direct dependency, why it is used and its licence |
| [releasing.md](releasing.md) | How changelog entries are written and how a release turns them into release notes |
| [benchmarks/](benchmarks/README.md) | How local models perform on 8, 16 and 32 GB machines, how to run the benchmark suite, and how the mistral.rs decision will be made |

## Architecture

| Doc | What it covers |
|---|---|
| [adr/](adr/README.md) | Architecture decision records: crate layout, desktop shell, inference, artifacts, agents and skills, MCP tools, storage and secrets, security baseline, design system, model sources |

## Design

| Doc | What it covers |
|---|---|
| [design/design-system.md](design/design-system.md) | Tokens, components, states, writing and accessibility rules |
| [design/preview.html](design/preview.html) | A preview of the tokens and components |
| [design/dn-sync.md](design/dn-sync.md) | How devices and a Demios Node keep one data set: records, clock, deletes, conflicts, the group key, and Relay mailboxes (draft) |

## Security and privacy

| Doc | What it covers |
|---|---|
| [../SECURITY.md](../SECURITY.md) | How to report a vulnerability |
| [security/mcp-tools.md](security/mcp-tools.md) | Threat model for MCP tools and approvals |
| [security/artifact-sandbox.md](security/artifact-sandbox.md) | Threat model for the artifact sandbox |

## Distribution

| Doc | What it covers |
|---|---|
| [store/](store/README.md) | Store and signing readiness: the audit, its findings and the proposed fixes. Store submissions are deferred; the first releases are installers on GitHub Releases and demios.io |
| [store/review-notes.md](store/review-notes.md) | Draft notes for store reviewers |
| [store/updates.md](store/updates.md) | Signed updates for direct downloads: how they work, the key setup and what a release publishes |
| [../packaging/msix/README.md](../packaging/msix/README.md) | Building the Microsoft Store (MSIX) package (submission deferred) |
