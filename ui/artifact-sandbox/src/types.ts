// TypeScript mirror of `dn_core::artifact` (crates/dn-core/src/artifact.rs) as serialised by serde.
// Rust owns these contracts; keep this file in step until dn-core exports TS bindings (ADR-0002).

export type ArtifactType =
  | { kind: "html" }
  | { kind: "react" }
  | { kind: "svg" }
  | { kind: "mermaid" }
  | { kind: "markdown" }
  | { kind: "code"; language: string };

export type ArtifactKind = ArtifactType["kind"];

export type Author = "model" | "user";

export interface ArtifactVersion {
  /** 1-based, strictly increasing per artifact. */
  n: number;
  content: string;
  author: Author;
  /** `null` for v1; a rollback points at the restored version. */
  parent_n: number | null;
  message_id: string | null;
  /** RFC 3339. */
  created_at: string;
}

export interface Artifact {
  id: string;
  conversation_id: string;
  artifact_type: ArtifactType;
  title: string;
  versions: ArtifactVersion[];
}

export type ArtifactOp =
  | { op: "create"; artifact_type: ArtifactType; title: string; content: string }
  | { op: "update"; id: string; old_str: string; new_str: string }
  | { op: "rewrite"; id: string; content: string };

/** A problem reported by the sandbox, shown under the artifact and sent to the model by "Fix this". */
export interface SandboxIssue {
  /** `network`: a fetch was blocked by the no-network policy (the pane offers the toggle). */
  kind: "error" | "unhandledrejection" | "console" | "csp" | "network" | "render";
  message: string;
  /** Script position or stack, when the browser provides one. */
  detail?: string;
}

export function latest(a: Artifact): ArtifactVersion {
  const v = a.versions[a.versions.length - 1];
  if (!v) throw new Error(`artifact ${a.id} has no versions`);
  return v;
}
