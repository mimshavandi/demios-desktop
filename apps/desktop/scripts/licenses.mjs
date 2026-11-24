// Builds ui/src/generated/licenses.json for Settings › About › Open-source licences
// (T-STORE-07). The UI bundles it as a lazily loaded module, so no fetch and no CSP change.
//
// Sources:
// - Rust: `cargo about generate --format json` on the app's dependency graph (normal deps only;
//   about.toml at the repo root, whose `accepted` list mirrors deny.toml).
// - npm: scripts/npm-licenses.mjs (production dependencies only).
// - licenses-extra.json: C/C++ code compiled in from -sys crates that cargo-about can't see
//   (llama.cpp/ggml and what it vendors, SQLite).
//
// Fails when a package has no licence text or a licence outside the accepted list.
//
// Usage:
//   node scripts/licenses.mjs            generate (fails if cargo-about is missing)
//   node scripts/licenses.mjs --if-available
//                                        skip with a warning when cargo-about is missing, unless
//                                        this is a release build: `tauri build` without --debug
//                                        (TAURI_ENV_DEBUG=false) or DEMIOS_REQUIRE_LICENSES=1
//   node scripts/licenses.mjs --check    generate into memory and validate only (CI)

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { npmLicences } from "./npm-licenses.mjs";

const appDir = fileURLToPath(new URL("..", import.meta.url));
const repoDir = join(appDir, "..", "..");
const outFile = join(appDir, "ui", "src", "generated", "licenses.json");

/** A release build must ship the list; dev, test and debug builds may skip it. */
export function listRequired(env = process.env, args = new Set()) {
  if (env.DEMIOS_REQUIRE_LICENSES === "1") return true;
  // `tauri build` sets TAURI_ENV_DEBUG for beforeBuildCommand: "false" for release builds.
  if (env.TAURI_ENV_DEBUG === "false") return true;
  return !args.has("--if-available");
}

/**
 * SPDX ids accepted for Rust and npm packages: about.toml's top-level `accepted` (mirrors
 * deny.toml), plus per-crate exceptions from its `[<crate>] accepted = [...]` tables.
 */
export function acceptedIds(toml = readFileSync(join(repoDir, "about.toml"), "utf8")) {
  const ids = (list) => new Set([...list.matchAll(/"([^"]+)"/g)].map((x) => x[1]));
  const top = toml.split(/^\[/m)[0].match(/^accepted\s*=\s*\[([\s\S]*?)\]/m);
  if (!top) throw new Error("about.toml has no top-level accepted = [...] list");
  const perCrate = new Map();
  for (const m of toml.matchAll(/^\[([A-Za-z0-9_-]+)\]\s*\n\s*accepted\s*=\s*\[([\s\S]*?)\]/gm)) {
    perCrate.set(m[1], ids(m[2]));
  }
  return { all: ids(top[1]), perCrate };
}

/** Licences of hand-listed C/C++ code, reviewed when added (not SPDX-common, so listed here). */
const EXTRA_ACCEPTED = new Set(["MIT", "Unlicense", "blessing"]);

/**
 * Whether an SPDX expression is satisfiable with `accepted`: any alternative of an OR, every
 * part of an AND. Handles the forms our dependencies use (parentheses, OR, AND, WITH).
 */
export function satisfied(expr, accepted) {
  let s = expr.trim();
  while (s.startsWith("(") && matchingParen(s, 0) === s.length - 1) s = s.slice(1, -1).trim();
  const or = splitTop(s, " OR ");
  if (or.length > 1) return or.some((p) => satisfied(p, accepted));
  const and = splitTop(s, " AND ");
  if (and.length > 1) return and.every((p) => satisfied(p, accepted));
  return s !== "" && accepted.has(s);
}

function matchingParen(s, open) {
  let depth = 0;
  for (let i = open; i < s.length; i++) {
    if (s[i] === "(") depth++;
    else if (s[i] === ")" && --depth === 0) return i;
  }
  return -1;
}

function splitTop(s, sep) {
  const parts = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "(") depth++;
    else if (s[i] === ")") depth--;
    else if (depth === 0 && s.startsWith(sep, i)) {
      parts.push(s.slice(start, i));
      start = i + sep.length;
      i += sep.length - 1;
    }
  }
  parts.push(s.slice(start));
  return parts;
}

function cargoAboutAvailable() {
  try {
    execFileSync("cargo", ["about", "--version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

function rustLicences() {
  const json = execFileSync(
    "cargo",
    [
      "about",
      "generate",
      "--format",
      "json",
      "--locked",
      "--offline",
      "--manifest-path",
      join(appDir, "Cargo.toml"),
    ],
    { cwd: repoDir, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, stdio: ["ignore", "pipe", "inherit"] },
  );
  const about = JSON.parse(json);
  const texts = new Map(); // crate id -> texts
  for (const l of about.licenses) {
    for (const u of l.used_by) {
      const key = `${u.crate.name}@${u.crate.version}`;
      if (!texts.has(key)) texts.set(key, []);
      texts.get(key).push(l.text);
    }
  }
  return about.crates.map((c) => ({
    name: c.package.name,
    version: c.package.version,
    licence: c.license,
    texts: texts.get(`${c.package.name}@${c.package.version}`) ?? [],
  }));
}

/** Packages that must be listed: the big C/C++ code we compile in (regressions in extras). */
export const REQUIRED = ["llama.cpp and ggml", "SQLite"];

export function missingRequired(data, required = REQUIRED) {
  return required.filter((n) => !data.packages.some((p) => p.name === n));
}

/** One entry per name@version (several npm roots can share packages). */
export function dedupe(list) {
  const seen = new Map();
  for (const p of list) if (!seen.has(`${p.name}@${p.version}`)) seen.set(`${p.name}@${p.version}`, p);
  return [...seen.values()];
}

/** Combines and validates; texts are stored once and referenced by index. */
export function build({ rust, npm, extra, accepted }) {
  const problems = [];
  const texts = [];
  const index = new Map();
  const ref = (t) => {
    const h = createHash("sha256").update(t).digest("hex");
    if (!index.has(h)) {
      index.set(h, texts.length);
      texts.push(t);
    }
    return index.get(h);
  };
  const packages = [];
  const add = (source, list, ok) => {
    for (const p of list) {
      const label = `${source} ${p.name} ${p.version}`;
      if (!p.licence) problems.push(`${label}: no licence declared`);
      else if (!ok(p.licence, p)) problems.push(`${label}: licence "${p.licence}" is not accepted`);
      const t = p.texts.filter((x) => x.trim() !== "");
      if (t.length === 0) problems.push(`${label}: no licence text`);
      packages.push({ name: p.name, version: p.version, source, licence: p.licence, texts: t.map(ref) });
    }
  };
  const forCrate = (name) => new Set([...accepted.all, ...(accepted.perCrate.get(name) ?? [])]);
  add("rust", rust, (l, p) => satisfied(l, forCrate(p.name)));
  add("npm", npm, (l) => satisfied(l, accepted.all));
  add("bundled", extra, (l) => satisfied(l, new Set([...accepted.all, ...EXTRA_ACCEPTED])));
  packages.sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
  return { problems, data: { packages, texts } };
}

function main() {
  const args = new Set(process.argv.slice(2));
  const required = listRequired(process.env, args);
  if (!cargoAboutAvailable()) {
    const msg =
      "cargo-about is not installed; the licences screen will say the list isn't in this build. " +
      "Install it with: cargo install cargo-about --locked --version 0.9.3 --features cli";
    if (required) {
      console.error(`error: ${msg}`);
      process.exit(1);
    }
    console.warn(`warning: ${msg}`);
    return;
  }
  const extra = JSON.parse(readFileSync(join(appDir, "licenses-extra.json"), "utf8")).packages.map((p) => ({
    ...p,
    texts: p.text_files.map((f) => readFileSync(join(appDir, f), "utf8")),
  }));
  // Each npm root whose code ships: the UI, plus any listed in licenses-extra.json (e.g. the
  // artifact sandbox once it has its own package.json). Missing roots are skipped.
  const extraRoots = JSON.parse(readFileSync(join(appDir, "licenses-extra.json"), "utf8")).npm_roots ?? [];
  const npm = [appDir, ...extraRoots.map((r) => join(appDir, r)).filter((r) => existsSync(join(r, "package.json")))]
    .flatMap((r) => npmLicences(r));
  const { problems, data } = build({ rust: rustLicences(), npm: dedupe(npm), extra, accepted: acceptedIds() });
  for (const n of missingRequired(data)) problems.push(`${n} is missing from the list`);
  if (problems.length > 0) {
    console.error(`Licence check failed:\n  ${problems.join("\n  ")}`);
    process.exit(1);
  }
  console.log(`licences: ${data.packages.length} packages, ${data.texts.length} distinct texts`);
  if (args.has("--check")) return;
  mkdirSync(dirname(outFile), { recursive: true });
  writeFileSync(outFile, JSON.stringify(data));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();
