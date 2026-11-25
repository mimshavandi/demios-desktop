// Decides how a release job signs (T-REL-02, #120), from which secrets the `release`
// environment has. It never prints a value, only names.
//
//   node scripts/release/signing-check.mjs macos|windows|updater
//
// - macos / windows: every secret present → `mode=signed`; none → `mode=unsigned` and
//   `suffix=-unsigned` (the asset names and the run summary say so: a labelled unsigned build,
//   not a silent one). Some but not all → the run fails and names the missing ones, so a
//   half-configured account never ships unsigned by accident.
// - updater: all three required. A release without the updater key isn't built.
// - Extra checks: APPLE_TEAM_ID (optional) must match the team in APPLE_SIGNING_IDENTITY;
//   AZURE_SIGNING_ENDPOINT must be an https Azure code-signing endpoint.
//
// In GitHub Actions the result goes to $GITHUB_OUTPUT (`mode`, `suffix`); it is also printed.

import { appendFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const REQUIRED = {
  macos: [
    "APPLE_CERTIFICATE",
    "APPLE_CERTIFICATE_PASSWORD",
    "APPLE_SIGNING_IDENTITY",
    "APPLE_API_KEY_P8",
    "APPLE_API_KEY",
    "APPLE_API_ISSUER",
  ],
  windows: [
    "AZURE_CLIENT_ID",
    "AZURE_TENANT_ID",
    "AZURE_CLIENT_SECRET",
    "AZURE_SIGNING_ENDPOINT",
    "AZURE_SIGNING_ACCOUNT",
    "AZURE_SIGNING_PROFILE",
  ],
  updater: ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD", "DEMIOS_UPDATER_PUBKEY"],
};

const set = (env, k) => typeof env[k] === "string" && env[k].trim() !== "";

/** `{ mode: "signed" | "unsigned", suffix }`, or throws with the names that are missing or wrong. */
export function signingMode(platform, env) {
  const names = REQUIRED[platform];
  if (names === undefined) throw new Error(`unknown platform ${platform} (macos, windows or updater)`);
  const missing = names.filter((k) => !set(env, k));
  if (platform === "updater") {
    if (missing.length > 0) throw new Error(`the updater needs ${missing.join(", ")}: a release isn't built without its update key`);
    return { mode: "signed", suffix: "" };
  }
  if (missing.length === names.length) return { mode: "unsigned", suffix: "-unsigned" };
  if (missing.length > 0) {
    throw new Error(`${platform} signing is only partly set up: missing ${missing.join(", ")} in the release environment`);
  }
  if (platform === "macos" && set(env, "APPLE_TEAM_ID")) {
    const team = env.APPLE_TEAM_ID.trim();
    if (!env.APPLE_SIGNING_IDENTITY.includes(`(${team})`)) {
      throw new Error("APPLE_SIGNING_IDENTITY is not an identity of APPLE_TEAM_ID (expected \"Developer ID Application: … (TEAMID)\")");
    }
  }
  if (platform === "macos" && !env.APPLE_SIGNING_IDENTITY.startsWith("Developer ID Application:")) {
    throw new Error("APPLE_SIGNING_IDENTITY must be a \"Developer ID Application: …\" identity for direct downloads");
  }
  if (platform === "windows") {
    let u;
    try {
      u = new URL(env.AZURE_SIGNING_ENDPOINT.trim());
    } catch {
      throw new Error("AZURE_SIGNING_ENDPOINT is not a URL");
    }
    if (u.protocol !== "https:" || !u.hostname.endsWith(".codesigning.azure.net")) {
      throw new Error("AZURE_SIGNING_ENDPOINT must be https://<region>.codesigning.azure.net");
    }
  }
  return { mode: "signed", suffix: "" };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const platform = process.argv[2];
  try {
    const r = signingMode(platform, process.env);
    console.log(`${platform}: ${r.mode}`);
    if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `mode=${r.mode}\nsuffix=${r.suffix}\n`);
  } catch (e) {
    console.error(`::error::${e.message}`);
    process.exit(1);
  }
}
