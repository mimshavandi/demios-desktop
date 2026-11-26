#!/usr/bin/env node
// Signs the Connect catalogue (ADR-0018): writes the Ed25519 signature of the exact bytes of
// apps/desktop/connect/catalogue.json, hex-encoded, to catalogue.json.sig next to it.
//
//   node scripts/connect/sign-catalogue.mjs --test
//   node scripts/connect/sign-catalogue.mjs <private-key.pem>
//
// --test makes a new throwaway key pair, signs with it, writes its public half to
// connect/test-key.pub and drops the private half: no test private key is ever stored. Only
// debug builds with the `connect-test-key` feature trust that key.
// <private-key.pem> is a PKCS#8 PEM Ed25519 private key kept outside the repository.
// Zero dependencies (node:crypto).
import { createPrivateKey, createPublicKey, generateKeyPairSync, sign } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const arg = process.argv[2];
if (!arg) {
  console.error("usage: node scripts/connect/sign-catalogue.mjs --test | <private-key.pem>");
  process.exit(2);
}
const dir = join(dirname(fileURLToPath(import.meta.url)), "../../apps/desktop/connect");
const catalogue = readFileSync(join(dir, "catalogue.json"));
JSON.parse(catalogue.toString("utf8")); // refuse to sign something that isn't JSON
const key = arg === "--test" ? generateKeyPairSync("ed25519").privateKey : createPrivateKey(readFileSync(arg));
if (key.asymmetricKeyType !== "ed25519") {
  console.error(`not an Ed25519 key: ${key.asymmetricKeyType}`);
  process.exit(2);
}
const sig = sign(null, catalogue, key);
writeFileSync(join(dir, "catalogue.json.sig"), sig.toString("hex") + "\n");
const pub = createPublicKey(key).export({ format: "der", type: "spki" }).subarray(-32).toString("hex");
if (arg === "--test") writeFileSync(join(dir, "test-key.pub"), pub + "\n");
console.log(`signed ${catalogue.length} bytes; public key ${pub}`);
