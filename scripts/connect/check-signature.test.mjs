// node --test scripts/connect/*.test.mjs
import { generateKeyPairSync, sign } from "node:crypto";
import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";
import { checkRelease, rawPublicKey, REPO } from "./lib.mjs";

const pair = () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  return { privateKey, pub: rawPublicKey(publicKey).toString("hex") };
};
const catalogue = Buffer.from('{"format":1,"serial":1,"published":"2026-11-26","entries":[]}\n');

function dir({ pub = "", sig = "", body = catalogue } = {}) {
  const d = mkdtempSync(join(tmpdir(), "connect-check-"));
  writeFileSync(join(d, "catalogue.json"), body);
  writeFileSync(join(d, "catalogue.pub"), pub);
  writeFileSync(join(d, "catalogue.json.sig"), sig);
  return d;
}
const sigOf = (key, body = catalogue) => sign(null, body, key).toString("hex") + "\n";

test("no key yet: the release goes ahead and Connect stays off", () => {
  const r = checkRelease(dir());
  assert.equal(r.ok, true);
  assert.match(r.message, /not available/);
});

test("signed with the current key: ok", () => {
  const k = pair();
  assert.equal(checkRelease(dir({ pub: k.pub + "\n", sig: sigOf(k.privateKey) })).ok, true);
});

test("missing signature: refused", () => {
  const k = pair();
  const r = checkRelease(dir({ pub: k.pub }));
  assert.equal(r.ok, false);
  assert.match(r.message, /isn't signed/);
});

test("stale signature (catalogue changed after signing): refused", () => {
  const k = pair();
  const r = checkRelease(dir({ pub: k.pub, sig: sigOf(k.privateKey), body: Buffer.from(catalogue.toString().replace("1,", "2,")) }));
  assert.equal(r.ok, false);
  assert.match(r.message, /changed after it was signed, or it was signed with another key/);
});

test("signed with another key, or the test key: refused", () => {
  const owner = pair();
  const other = pair();
  assert.equal(checkRelease(dir({ pub: owner.pub, sig: sigOf(other.privateKey) })).ok, false);
  const testKey = pair();
  const r = checkRelease(dir({ pub: owner.pub, sig: sigOf(testKey.privateKey) }), testKey.pub);
  assert.equal(r.ok, false);
  assert.match(r.message, /TEST key/);
});

test("a malformed key or signature: refused", () => {
  assert.equal(checkRelease(dir({ pub: "abc" })).ok, false);
  const k = pair();
  assert.equal(checkRelease(dir({ pub: k.pub, sig: "zz" })).ok, false);
});

test("new-key refuses a path inside the repository", () => {
  const r = spawnSync(process.execPath, [join(REPO, "scripts/connect/new-key.mjs"), join(REPO, "key.pem")], { encoding: "utf8" });
  assert.equal(r.status, 2);
  assert.match(r.stderr, /inside the repository/);
});

test("the repository's own catalogue passes the release check", () => {
  const r = spawnSync(process.execPath, [join(REPO, "scripts/connect/check-signature.mjs")], { encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
});
