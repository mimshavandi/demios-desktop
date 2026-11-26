#!/usr/bin/env node
// Makes the owner's Connect catalogue key (ADR-0018). The private key goes to a path you
// choose OUTSIDE the repository and never into CI or repo secrets; the public key is written
// to apps/desktop/connect/catalogue.pub (commit that file) and printed.
//
//   node scripts/connect/new-key.mjs <path-outside-the-repo>/demios-catalogue-key.pem
//
// Refuses a path inside the repository and an existing file. Zero dependencies (node:crypto).
import { generateKeyPairSync } from "node:crypto";
import { existsSync, realpathSync, writeFileSync } from "node:fs";
import { basename, dirname, join, relative, resolve, isAbsolute } from "node:path";
import { CONNECT_DIR, REPO, rawPublicKey } from "./lib.mjs";

const out = process.argv[2];
if (!out) {
  console.error("usage: node scripts/connect/new-key.mjs <path-outside-the-repo>.pem");
  process.exit(2);
}
// Through symlinks too: the folder as it really is on disk.
let path;
try {
  path = join(realpathSync(dirname(resolve(out))), basename(out));
} catch {
  console.error(`The folder for ${out} doesn't exist.`);
  process.exit(2);
}
const rel = relative(realpathSync(REPO), path);
if (!rel.startsWith("..") && !isAbsolute(rel)) {
  console.error("Refusing to write the private key inside the repository. Choose a path outside it.");
  process.exit(2);
}
if (existsSync(path)) {
  console.error(`${path} already exists. Refusing to overwrite a key.`);
  process.exit(2);
}
const { privateKey, publicKey } = generateKeyPairSync("ed25519");
writeFileSync(path, privateKey.export({ format: "pem", type: "pkcs8" }), { mode: 0o600, flag: "wx" });
const pub = rawPublicKey(publicKey).toString("hex");
writeFileSync(join(CONNECT_DIR, "catalogue.pub"), pub + "\n");
console.log(`Private key: ${path} (keep it on your own hardware; never commit it or add it to CI).`);
console.log(`Public key:  ${pub}`);
console.log("Written to apps/desktop/connect/catalogue.pub: commit it, then sign the catalogue.");
