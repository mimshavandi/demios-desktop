// In-memory mirror of dn-artifacts' store semantics, for the demo page and UI tests. The real
// store is Rust (crates/dn-artifacts); both are checked against
// crates/dn-artifacts/tests/vectors/update.json.

import { latest, type Artifact, type ArtifactOp, type ArtifactVersion, type Author } from "./types";

export type StoreErrorCode = "not_found" | "version_not_found" | "empty_old_str" | "no_match" | "ambiguous";

export class StoreError extends Error {
  constructor(
    readonly code: StoreErrorCode,
    message: string,
    readonly count?: number,
  ) {
    super(message);
  }
}

/** Overlapping occurrences of `needle`, and the first offset. */
export function countMatches(hay: string, needle: string): { count: number; first: number } {
  let count = 0;
  let first = -1;
  let from = 0;
  for (;;) {
    const at = hay.indexOf(needle, from);
    if (at < 0) break;
    if (first < 0) first = at;
    count++;
    const cp = hay.codePointAt(at) ?? 0;
    from = at + (cp > 0xffff ? 2 : 1);
  }
  return { count, first };
}

/** Artifacts are stored with LF line endings (same as dn-artifacts). */
export function normaliseNewlines(s: string): string {
  return s.replace(/\r\n?/g, "\n");
}

export function applyUpdate(content: string, oldStr: string, newStr: string): string {
  if (oldStr === "") throw new StoreError("empty_old_str", "old_str must not be empty");
  content = normaliseNewlines(content);
  oldStr = normaliseNewlines(oldStr);
  newStr = normaliseNewlines(newStr);
  const { count, first } = countMatches(content, oldStr);
  if (count === 0) throw new StoreError("no_match", "old_str was not found");
  if (count > 1) throw new StoreError("ambiguous", `old_str matches ${count} times`, count);
  return content.slice(0, first) + newStr + content.slice(first + oldStr.length);
}

export class MemoryArtifactStore {
  private readonly items = new Map<string, Artifact>();
  private seq = 0;

  apply(conversationId: string, op: ArtifactOp, author: Author = "model"): Artifact {
    switch (op.op) {
      case "create": {
        const id = `art-${++this.seq}`;
        const a: Artifact = {
          id,
          conversation_id: conversationId,
          artifact_type: op.artifact_type,
          title: op.title,
          versions: [this.version(1, op.content, author, null)],
        };
        this.items.set(id, a);
        return structuredClone(a);
      }
      case "update": {
        const a = this.get(op.id, conversationId);
        const prev = latest(a);
        return this.append(a, applyUpdate(prev.content, op.old_str, op.new_str), author, prev.n);
      }
      case "rewrite": {
        const a = this.get(op.id, conversationId);
        return this.append(a, op.content, author, latest(a).n);
      }
    }
  }

  rollback(id: string, toN: number, author: Author = "user"): Artifact {
    const a = this.items.get(id);
    if (!a) throw new StoreError("not_found", `artifact ${id} not found`);
    const v = a.versions.find((x) => x.n === toN);
    if (!v) throw new StoreError("version_not_found", `artifact ${id} has no version ${toN}`);
    return this.append(a, v.content, author, toN);
  }

  list(conversationId: string): Artifact[] {
    return [...this.items.values()].filter((a) => a.conversation_id === conversationId).map((a) => structuredClone(a));
  }

  private get(id: string, conversationId: string): Artifact {
    const a = this.items.get(id);
    if (!a || a.conversation_id !== conversationId) throw new StoreError("not_found", `artifact ${id} not found`);
    return a;
  }

  private append(a: Artifact, content: string, author: Author, parent: number): Artifact {
    a.versions.push(this.version(latest(a).n + 1, content, author, parent));
    return structuredClone(a);
  }

  private version(n: number, content: string, author: Author, parent: number | null): ArtifactVersion {
    return { n, content, author, parent_n: parent, message_id: null, created_at: new Date().toISOString() };
  }
}
