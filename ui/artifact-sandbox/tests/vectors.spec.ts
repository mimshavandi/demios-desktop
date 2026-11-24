// The demo's in-memory store must patch exactly like the Rust store (crates/dn-artifacts).
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { MemoryArtifactStore, StoreError, applyUpdate } from "../src/memory-store";

interface Case {
  name: string;
  content: string;
  old_str: string;
  new_str: string;
  expect: { ok?: string; error?: string; count?: number };
}

const file = fileURLToPath(new URL("../../../crates/dn-artifacts/tests/vectors/update.json", import.meta.url));
const { cases } = JSON.parse(readFileSync(file, "utf8")) as { cases: Case[] };

test.describe("update vectors shared with dn-artifacts", () => {
  for (const c of cases) {
    test(c.name, () => {
      if (c.expect.ok !== undefined) {
        expect(applyUpdate(c.content, c.old_str, c.new_str)).toBe(c.expect.ok);
        return;
      }
      let err: unknown;
      try {
        applyUpdate(c.content, c.old_str, c.new_str);
      } catch (e) {
        err = e;
      }
      expect(err).toBeInstanceOf(StoreError);
      expect((err as StoreError).code).toBe(c.expect.error);
      if (c.expect.count !== undefined) expect((err as StoreError).count).toBe(c.expect.count);
    });
  }
});

test("rollback appends a version whose parent is the restored one", () => {
  const s = new MemoryArtifactStore();
  const a = s.apply("c", { op: "create", artifact_type: { kind: "markdown" }, title: "t", content: "v1" });
  s.apply("c", { op: "update", id: a.id, old_str: "v1", new_str: "v2" });
  const r = s.rollback(a.id, 1);
  expect(r.versions.map((v) => [v.n, v.parent_n, v.content])).toEqual([
    [1, null, "v1"],
    [2, 1, "v2"],
    [3, 1, "v1"],
  ]);
  expect(() => s.apply("other", { op: "rewrite", id: a.id, content: "x" })).toThrow(StoreError);
});
