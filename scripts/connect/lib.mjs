// Shared by the Connect catalogue scripts (ADR-0018). Zero dependencies (node:crypto).
import { createPublicKey, verify as edVerify } from "node:crypto";
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO = join(dirname(fileURLToPath(import.meta.url)), "../..");
export const CONNECT_DIR = join(REPO, "apps/desktop/connect");

// The SPKI DER prefix of an Ed25519 public key; the raw 32-byte key follows it.
const SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

export const rawPublicKey = (keyObject) => keyObject.export({ format: "der", type: "spki" }).subarray(-32);
export const publicKeyFromHex = (hex) =>
  createPublicKey({ key: Buffer.concat([SPKI_PREFIX, Buffer.from(hex, "hex")]), format: "der", type: "spki" });

export const read = (dir, name) => {
  const p = join(dir, name);
  return existsSync(p) ? readFileSync(p) : null;
};

const isHex = (s, len) => s.length === len && /^[0-9a-f]+$/i.test(s);

/**
 * The release check: is `catalogue.json.sig` the owner's signature of `catalogue.json`, made
 * with the key in `catalogue.pub`? Returns `{ ok, message }`. No key yet is ok: release builds
 * then show Connect as not available.
 */
export function checkRelease(dir = CONNECT_DIR, testPublicHex = null) {
  const catalogue = read(dir, "catalogue.json");
  if (catalogue === null) return { ok: false, message: "apps/desktop/connect/catalogue.json is missing." };
  const pub = (read(dir, "catalogue.pub") ?? Buffer.alloc(0)).toString("utf8").trim();
  if (pub === "") {
    return { ok: true, message: "No catalogue key yet (connect/catalogue.pub is empty): this release shows Connect as not available." };
  }
  if (!isHex(pub, 64)) return { ok: false, message: "apps/desktop/connect/catalogue.pub isn't 64 hex digits." };
  const sig = (read(dir, "catalogue.json.sig") ?? Buffer.alloc(0)).toString("utf8").trim();
  const resign = "The owner signs it by hand: node scripts/connect/sign-catalogue.mjs <private-key.pem> (docs/releasing.md).";
  if (sig === "") return { ok: false, message: `The Connect catalogue isn't signed (apps/desktop/connect/catalogue.json.sig is empty). ${resign}` };
  if (!isHex(sig, 128)) return { ok: false, message: `apps/desktop/connect/catalogue.json.sig isn't a signature (128 hex digits). ${resign}` };
  const sigBytes = Buffer.from(sig, "hex");
  if (edVerify(null, catalogue, publicKeyFromHex(pub), sigBytes)) {
    return { ok: true, message: "The Connect catalogue is signed with the current key." };
  }
  if (testPublicHex !== null && edVerify(null, catalogue, publicKeyFromHex(testPublicHex), sigBytes)) {
    return { ok: false, message: `catalogue.json.sig was made with the TEST key, not the key in catalogue.pub. ${resign}` };
  }
  return {
    ok: false,
    message: `catalogue.json.sig doesn't match: catalogue.json changed after it was signed, or it was signed with another key than the one in catalogue.pub. ${resign}`,
  };
}
