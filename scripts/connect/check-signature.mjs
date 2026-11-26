#!/usr/bin/env node
// Release check (ADR-0018): refuses a release whose Connect catalogue isn't signed with the
// owner's current key. Needs no secret: it checks catalogue.json.sig against catalogue.pub.
//
//   node scripts/connect/check-signature.mjs
import { checkRelease, CONNECT_DIR, read } from "./lib.mjs";

const testPub = read(CONNECT_DIR, "test-key.pub")?.toString("utf8").trim() || null;
const { ok, message } = checkRelease(CONNECT_DIR, testPub);
if (ok) {
  console.log(message);
} else {
  console.error(`::error title=Connect catalogue::${message}`);
  process.exit(1);
}
