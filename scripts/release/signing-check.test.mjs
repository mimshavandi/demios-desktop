import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { REQUIRED, signingMode } from "./signing-check.mjs";

const all = (p, extra = {}) => ({ ...Object.fromEntries(REQUIRED[p].map((k) => [k, "x"])), ...extra });
const mac = (extra = {}) => all("macos", { APPLE_SIGNING_IDENTITY: "Developer ID Application: DigiNote Inc. (ABCDE12345)", ...extra });
const win = (extra = {}) => all("windows", { AZURE_SIGNING_ENDPOINT: "https://weu.codesigning.azure.net/", ...extra });

test("no secrets: a labelled unsigned build", () => {
  assert.deepEqual(signingMode("macos", {}), { mode: "unsigned", suffix: "-unsigned" });
  assert.deepEqual(signingMode("windows", { AZURE_CLIENT_ID: "  " }), { mode: "unsigned", suffix: "-unsigned" });
});

test("every secret: signed", () => {
  assert.deepEqual(signingMode("macos", mac()), { mode: "signed", suffix: "" });
  assert.deepEqual(signingMode("macos", mac({ APPLE_TEAM_ID: "ABCDE12345" })).mode, "signed");
  assert.deepEqual(signingMode("windows", win()), { mode: "signed", suffix: "" });
});

test("some secrets: the run fails and names only what is missing", () => {
  const env = mac();
  delete env.APPLE_API_ISSUER;
  env.APPLE_API_KEY_P8 = "";
  assert.throws(() => signingMode("macos", env), /partly set up: missing APPLE_API_KEY_P8, APPLE_API_ISSUER/);
  const w = win();
  delete w.AZURE_CLIENT_SECRET;
  assert.throws(() => signingMode("windows", w), /missing AZURE_CLIENT_SECRET in the release environment/);
});

test("identity and endpoint are checked", () => {
  assert.throws(() => signingMode("macos", mac({ APPLE_TEAM_ID: "ZZZZZ99999" })), /not an identity of APPLE_TEAM_ID/);
  assert.throws(() => signingMode("macos", mac({ APPLE_SIGNING_IDENTITY: "Apple Development: x (ABCDE12345)" })), /Developer ID Application/);
  assert.throws(() => signingMode("windows", win({ AZURE_SIGNING_ENDPOINT: "http://weu.codesigning.azure.net" })), /https/);
  assert.throws(() => signingMode("windows", win({ AZURE_SIGNING_ENDPOINT: "https://evil.example.com" })), /codesigning\.azure\.net/);
  assert.throws(() => signingMode("windows", win({ AZURE_SIGNING_ENDPOINT: "not a url" })), /not a URL/);
});

test("the updater key is required for a release", () => {
  assert.equal(signingMode("updater", all("updater")).mode, "signed");
  assert.throws(() => signingMode("updater", {}), /updater needs TAURI_SIGNING_PRIVATE_KEY, TAURI_SIGNING_PRIVATE_KEY_PASSWORD, DEMIOS_UPDATER_PUBKEY/);
  assert.throws(() => signingMode("linux", {}), /unknown platform/);
});

test("CLI writes GITHUB_OUTPUT and never prints a secret value", () => {
  const script = fileURLToPath(new URL("./signing-check.mjs", import.meta.url));
  const dir = mkdtempSync(join(tmpdir(), "signing-check-"));
  const out = join(dir, "out");
  const secret = "SUPER-SECRET-VALUE-123";
  const stdout = execFileSync(process.execPath, [script, "windows"], {
    env: { ...win({ AZURE_CLIENT_SECRET: secret }), GITHUB_OUTPUT: out, PATH: process.env.PATH },
    encoding: "utf8",
  });
  assert.equal(stdout.trim(), "windows: signed");
  assert.equal(readFileSync(out, "utf8"), "mode=signed\nsuffix=\n");
  let err = "";
  try {
    execFileSync(process.execPath, [script, "windows"], {
      env: { AZURE_CLIENT_SECRET: secret, PATH: process.env.PATH },
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (e) {
    err = String(e.stderr);
  }
  assert.match(err, /partly set up/);
  assert.ok(!err.includes(secret));
});
