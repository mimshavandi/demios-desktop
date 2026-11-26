# Architecture decision records

Format: Context → Decision → Options (table) → Trade-offs → Consequences → Action items (task IDs). Status moves `Proposed → Accepted → Superseded by ADR-xxxx`; accepted ADRs are never edited in substance, only superseded.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-repo-and-crate-layout.md) | Repo and crate layout | Proposed |
| [0002](0002-desktop-shell.md) | Desktop shell: Tauri 2 | Proposed |
| [0003](0003-inference-layer.md) | Inference layer: backend trait, llama.cpp default | Proposed |
| [0004](0004-artifacts-runtime.md) | Claude-style artifacts runtime | Proposed |
| [0005](0005-agent-skill-manifests.md) | Agents and skills: manifests + tool-use loop | Proposed |
| [0006](0006-tools-mcp-capabilities.md) | Tools: MCP + capability/approval layer | Proposed |
| [0007](0007-local-storage-and-secrets.md) | Local storage, knowledge index and secrets | Proposed |
| 0008 | Not public (proprietary add-ons) | – |
| [0009](0009-security-baseline.md) | Security and supply-chain baseline | Proposed |
| [0010](0010-design-system.md) | One design system: tokens + headless primitives | Accepted |
| [0011](0011-model-sources.md) | Model sources: Hugging Face Hub first, no in-house registry | Accepted |
| [0012](0012-windows-artifact-webview.md) | Artifacts on Windows render in a separate, capability-less webview | Accepted |
| [0013](0013-plugin-api.md) | Plugin API: in-process add-ons behind host services, same approvals, offline licences | Accepted; amendment proposed (§9) |
| [0014](0014-demios-node.md) | Demios Node: the engine run headless on the user's own machine or server, with paired clients | Accepted |
| [0015](0015-encrypted-sharing-and-forms.md) | Sharing artifacts through encrypted links, and forms that collect encrypted responses | Accepted |
| [0016](0016-demios-relay.md) | Demios Relay: encrypted store-and-forward mailboxes, addressed by public key | Accepted |
| [0017](0017-licence-polyform-shield.md) | Licence: PolyForm Shield 1.0.0 for the app, `dn`, the Node and the shared crates | Accepted |
| [0018](0018-connect-catalogue.md) | Connect: a curated catalogue of tool servers, with no connector code to maintain | Accepted |
