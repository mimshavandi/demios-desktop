// npm run test:scripts (T-STORE-07): the licence checks that gate CI.
import { test } from "node:test";
import assert from "node:assert/strict";
import { acceptedIds, applyOverrides, build, dedupe, excludeNpm, listRequired, missingRequired, npmExceptions, satisfied } from "./licenses.mjs";
import { npmLs } from "./npm-licenses.mjs";

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

test("release builds require the list; dev and debug builds may skip it", () => {
  const skip = new Set(["--if-available"]);
  assert.equal(listRequired({}, skip), false);
  assert.equal(listRequired({ TAURI_ENV_DEBUG: "true" }, skip), false);
  assert.equal(listRequired({ TAURI_ENV_DEBUG: "false" }, skip), true);
  assert.equal(listRequired({ DEMIOS_REQUIRE_LICENSES: "1" }, skip), true);
  assert.equal(listRequired({}, new Set()), true);
});

test("npm ls: a non-zero exit with JSON output is used, without JSON it fails", () => {
  const failing = (stdout) => () => {
    const e = new Error("exit 1");
    e.stdout = stdout;
    e.stderr = "npm error extraneous: x";
    throw e;
  };
  assert.equal(npmLs(".", failing('{"dependencies":{}}')), '{"dependencies":{}}');
  assert.throws(() => npmLs(".", failing("")), /exit 1/);
});

test("packages shared by several npm roots are listed once", () => {
  const p = { name: "a", version: "1", licence: "MIT", texts: ["t"] };
  assert.equal(dedupe([p, { ...p }, { ...p, version: "2" }]).length, 2);
});

test("excluded npm packages (not shipped) need a reason and are left out", () => {
  const list = [{ name: "elkjs", version: "1", licence: "EPL-2.0 OR GPL-3.0", texts: ["t"] }, { name: "a", version: "1", licence: "MIT", texts: ["t"] }];
  assert.deepEqual(excludeNpm(list, [{ name: "elkjs", reason: "stubbed at build time" }]).map((p) => p.name), ["a"]);
  assert.throws(() => excludeNpm(list, [{ name: "elkjs" }]), /need a name and a reason/);
});

test("npm exceptions apply to the named package only", () => {
  const exc = npmExceptions([{ name: "robust-predicates", accepted: ["Unlicense"] }]);
  const { problems } = build({
    rust: [],
    npm: [pkg("robust-predicates", "Unlicense"), pkg("other", "Unlicense")],
    extra: [],
    accepted: ok,
    npmExceptions: exc,
  });
  assert.deepEqual(problems, ['npm other 1.0.0: licence "Unlicense" is not accepted']);
});

test("licence overrides fill only a missing field, for the exact version", () => {
  const o = [{ name: "khroma", version: "2.1.0", licence: "MIT" }];
  const list = [
    { name: "khroma", version: "2.1.0", licence: "", texts: ["t"] },
    { name: "khroma", version: "2.2.0", licence: "", texts: ["t"] },
    { name: "x", version: "1", licence: "GPL-3.0", texts: ["t"] },
  ];
  assert.deepEqual(applyOverrides(list, [...o, { name: "x", version: "1", licence: "MIT" }]).map((p) => p.licence), ["MIT", "", "GPL-3.0"]);
});

test("a dependency that isn't installed fails instead of being skipped", async () => {
  const { npmLicences } = await import("./npm-licenses.mjs");
  const tree = JSON.stringify({ dependencies: { mermaid: { version: "12.3.0", missing: true } } });
  assert.throws(() => npmLicences("/x", () => tree), /mermaid is not installed in \/x/);
});
