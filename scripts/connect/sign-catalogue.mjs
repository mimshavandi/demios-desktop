#!/usr/bin/env node
// Signs the Connect catalogue (ADR-0018): the Ed25519 signature of the exact bytes of
// apps/desktop/connect/catalogue.json, hex-encoded.
//
//   node scripts/connect/sign-catalogue.mjs <private-key.pem>   the owner, by hand:
//       writes catalogue.json.sig; the key must match catalogue.pub.
//   node scripts/connect/sign-catalogue.mjs --test               anyone, after editing:
//       makes a new throwaway test key pair, writes catalogue.json.test-sig and the key's
//       public half to test-key.pub, and drops the private half (no test private key is ever
//       stored). Only debug builds with the `connect-test-key` feature trust it.
//
// Zero dependencies (node:crypto).
import { createPrivateKey, createPublicKey, generateKeyPairSync, sign } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { checkRelease, CONNECT_DIR, rawPublicKey } from "./lib.mjs";

const arg = process.argv[2];
if (!arg) {
  console.error("usage: node scripts/connect/sign-catalogue.mjs <private-key.pem> | --test");
  process.exit(2);
}
const test = arg === "--test";
const catalogue = readFileSync(join(CONNECT_DIR, "catalogue.json"));
JSON.parse(catalogue.toString("utf8")); // refuse to sign something that isn't JSON
const key = test ? generateKeyPairSync("ed25519").privateKey : createPrivateKey(readFileSync(arg));
if (key.asymmetricKeyType !== "ed25519") {
  console.error(`not an Ed25519 key: ${key.asymmetricKeyType}`);
  process.exit(2);
}
const pub = rawPublicKey(createPublicKey(key)).toString("hex");
if (!test) {
  const expected = readFileSync(join(CONNECT_DIR, "catalogue.pub"), "utf8").trim();
  if (expected === "") {
    console.error("apps/desktop/connect/catalogue.pub is empty: make the key first (scripts/connect/new-key.mjs).");
    process.exit(2);
  }
  if (expected !== pub) {
    console.error(`This key (${pub}) isn't the one in catalogue.pub (${expected}).`);
    process.exit(2);
  }
}
const out = test ? "catalogue.json.test-sig" : "catalogue.json.sig";
writeFileSync(join(CONNECT_DIR, out), sign(null, catalogue, key).toString("hex") + "\n");
if (test) writeFileSync(join(CONNECT_DIR, "test-key.pub"), pub + "\n");
console.log(`Signed ${catalogue.length} bytes into apps/desktop/connect/${out} (public key ${pub}).`);
if (!test) {
  const { ok, message } = checkRelease();
  console.log(message);
  if (!ok) process.exit(1);
}
