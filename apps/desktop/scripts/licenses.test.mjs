// npm run test:scripts (T-STORE-07): the licence checks that gate CI.
import { test } from "node:test";
import assert from "node:assert/strict";
import { acceptedIds, build, missingRequired, satisfied } from "./licenses.mjs";

const accepted = new Set(["MIT", "Apache-2.0", "ISC", "Apache-2.0 WITH LLVM-exception"]);

test("SPDX expressions: OR needs one, AND needs all, parentheses and WITH", () => {
  assert.equal(satisfied("MIT", accepted), true);
  assert.equal(satisfied("GPL-3.0", accepted), false);
  assert.equal(satisfied("MIT OR GPL-3.0", accepted), true);
  assert.equal(satisfied("MIT AND GPL-3.0", accepted), false);
  assert.equal(satisfied("(MPL-2.0 OR Apache-2.0)", accepted), true);
  assert.equal(satisfied("ISC AND (Apache-2.0 OR ISC) AND MIT", accepted), true);
  assert.equal(satisfied("Apache-2.0 WITH LLVM-exception OR GPL-2.0", accepted), true);
  assert.equal(satisfied("", accepted), false);
});

test("about.toml: top-level list and per-crate exceptions", () => {
  const ids = acceptedIds(`accepted = ["MIT", "ISC"]\nignore-dev-dependencies = true\n\n[webpki-root-certs]\naccepted = ["CDLA-Permissive-2.0"]\n`);
  assert.deepEqual([...ids.all], ["MIT", "ISC"]);
  assert.deepEqual([...ids.perCrate.get("webpki-root-certs")], ["CDLA-Permissive-2.0"]);
});

const ok = { all: accepted, perCrate: new Map([["certs", new Set(["CDLA-Permissive-2.0"])]]) };
const pkg = (name, licence, texts = ["text"]) => ({ name, version: "1.0.0", licence, texts });

test("fails on a missing text or an unlisted licence; exceptions are per crate", () => {
  const { problems } = build({
    rust: [pkg("a", "MIT"), pkg("b", "GPL-3.0"), pkg("c", "MIT", []), pkg("certs", "CDLA-Permissive-2.0")],
    npm: [pkg("d", "MIT", ["  "]), pkg("certs", "CDLA-Permissive-2.0")],
    extra: [pkg("sqlite", "blessing")],
    accepted: ok,
  });
  assert.deepEqual(problems, [
    'rust b 1.0.0: licence "GPL-3.0" is not accepted',
    "rust c 1.0.0: no licence text",
    "npm d 1.0.0: no licence text",
    'npm certs 1.0.0: licence "CDLA-Permissive-2.0" is not accepted',
  ]);
});

test("texts are stored once and referenced by index", () => {
  const { problems, data } = build({
    rust: [pkg("a", "MIT", ["same"]), pkg("b", "MIT", ["same", "other"])],
    npm: [],
    extra: [],
    accepted: ok,
  });
  assert.deepEqual(problems, []);
  assert.deepEqual(data.texts, ["same", "other"]);
  assert.deepEqual(data.packages.map((p) => p.texts), [[0], [0, 1]]);
});

test("llama.cpp and SQLite must be listed", () => {
  const data = { packages: [{ name: "SQLite" }], texts: [] };
  assert.deepEqual(missingRequired(data), ["llama.cpp and ggml"]);
});
