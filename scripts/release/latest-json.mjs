// Writes the Tauri updater manifest (`latest.json`) for a release (T-REL-02, #120).
//
//   node scripts/release/latest-json.mjs --tag v0.2.0 --dir dist --repo owner/name \
//     [--changelog CHANGELOG.md] [--platforms darwin-aarch64,windows-x86_64] [--pub-date ISO]
//
// - Each update bundle in --dir needs its `.sig` next to it (written by `tauri build` with
//   TAURI_SIGNING_PRIVATE_KEY). A bundle without one fails the run: never a manifest entry
//   without a signature.
// - --platforms limits the manifest to platforms whose installers are code-signed, so an
//   installed, signed app is never offered an unsigned update. A listed platform without a
//   bundle fails the run.
// - The notes are the version's section of CHANGELOG.md (docs/releasing.md), as written.
//
// Bundle names (see .github/workflows/release.yml): Demios-Desktop-<version>-<os>-<arch>.<ext>
//   macos-arm64.app.tar.gz → darwin-aarch64, macos-x64.app.tar.gz → darwin-x86_64,
//   windows-x64-setup.exe → windows-x86_64, linux-x86_64.AppImage → linux-x86_64.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const VERSION = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/;

const PLATFORMS = [
  { platform: "darwin-aarch64", suffix: "-macos-arm64.app.tar.gz" },
  { platform: "darwin-x86_64", suffix: "-macos-x64.app.tar.gz" },
  { platform: "windows-x86_64", suffix: "-windows-x64-setup.exe" },
  { platform: "linux-x86_64", suffix: "-linux-x86_64.AppImage" },
];

/** The body of `## [<version>] …` in a Keep a Changelog file, without the heading. */
export function changelogSection(text, version) {
  const lines = text.split("\n");
  const start = lines.findIndex((l) => l.startsWith(`## [${version}]`));
  if (start < 0) throw new Error(`CHANGELOG has no section for ${version}`);
  let end = lines.findIndex((l, i) => i > start && (l.startsWith("## ") || /^\[[^\]]+\]:\s/.test(l)));
  if (end < 0) end = lines.length;
  const body = lines.slice(start + 1, end).join("\n").trim();
  if (body === "") throw new Error(`CHANGELOG section ${version} is empty`);
  return body;
}

/**
 * The manifest object. `files` maps file name → contents (only `.sig` contents are read).
 * `platforms`: the platforms to include (default: every bundle found).
 */
export function latestJson({ version, tag, repo, notes, pubDate, files, platforms }) {
  if (!VERSION.test(version)) throw new Error(`not a version: ${version}`);
  if (!/^v\d/.test(tag) || tag.replace(/^v/, "") !== version) throw new Error(`tag ${tag} doesn't match version ${version}`);
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo)) throw new Error(`not a repository: ${repo}`);
  const names = Object.keys(files);
  const out = { version, notes, pub_date: pubDate, platforms: {} };
  for (const p of PLATFORMS) {
    // The exact name only: a stale `…-0.2.0-rc.1-…` or a renamed copy is never picked up.
    const bundle = names.find((n) => n === `Demios-Desktop-${version}${p.suffix}`);
    const wanted = platforms === undefined ? bundle !== undefined : platforms.includes(p.platform);
    if (!wanted) continue;
    if (bundle === undefined) throw new Error(`${p.platform}: no update bundle (*${p.suffix})`);
    const sig = files[`${bundle}.sig`];
    if (sig === undefined || sig.trim() === "") throw new Error(`${p.platform}: ${bundle} has no .sig`);
    out.platforms[p.platform] = {
      signature: sig.trim(),
      url: `https://github.com/${repo}/releases/download/${tag}/${encodeURIComponent(bundle)}`,
    };
  }
  const unknown = (platforms ?? []).filter((x) => !PLATFORMS.some((p) => p.platform === x));
  if (unknown.length > 0) throw new Error(`unknown platform(s): ${unknown.join(", ")}`);
  if (Object.keys(out.platforms).length === 0) throw new Error("no platform to put in latest.json");
  return out;
}

/** A flag's value; a flag given without one (or followed by another flag) is an error. */
function arg(argv, name) {
  const i = argv.indexOf(name);
  if (i < 0) return undefined;
  const v = argv[i + 1];
  if (v === undefined || v.startsWith("--") || v.trim() === "") throw new Error(`${name} needs a value`);
  return v;
}

function main(argv) {
  const tag = arg(argv, "--tag");
  const dir = arg(argv, "--dir");
  const repo = arg(argv, "--repo");
  if (!tag || !dir || !repo) throw new Error("usage: --tag v<version> --dir <dist> --repo <owner/name>");
  const version = tag.replace(/^v/, "");
  if (!VERSION.test(version)) throw new Error(`not a version: ${version}`);
  const changelog = readFileSync(arg(argv, "--changelog") ?? "CHANGELOG.md", "utf8");
  const files = {};
  // Regular files only: a symlinked `.sig` never brings another file's contents in.
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    if (!e.isFile()) continue;
    files[e.name] = e.name.endsWith(".sig") ? readFileSync(join(dir, e.name), "utf8") : "";
  }
  const list = arg(argv, "--platforms");
  const manifest = latestJson({
    version,
    tag,
    repo,
    notes: changelogSection(changelog, version),
    pubDate: arg(argv, "--pub-date") ?? new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    files,
    ...(list === undefined ? {} : { platforms: list.split(",").filter(Boolean) }),
  });
  process.stdout.write(`${JSON.stringify(manifest, null, 2)}\n`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    main(process.argv.slice(2));
  } catch (e) {
    console.error(`latest-json: ${e.message}`);
    process.exit(1);
  }
}
