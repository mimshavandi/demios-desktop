// Licences of the npm packages that ship in the UI bundle (T-STORE-07). Zero dependencies:
// walks `npm ls --omit=dev --all --json --long` and reads each package's `license` field and
// its LICENSE / COPYING / NOTICE files from node_modules. Dev dependencies are not shipped and
// are left out.
//
// Standalone: `node scripts/npm-licenses.mjs` prints the list as JSON.

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const LICENCE_FILE = /^(licen[cs]e|copying|notice)([-._].*)?$/i;

/**
 * `npm ls` output. npm exits 1 when the tree has extraneous or invalid packages but still prints
 * the full JSON; use it then, and fail only when there is no JSON at all.
 */
export function npmLs(cwd, run = execFileSync) {
  try {
    return run("npm", ["ls", "--omit=dev", "--all", "--json", "--long"], {
      cwd,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "pipe"],
      // On Windows `npm` is a .cmd shim.
      shell: process.platform === "win32",
    });
  } catch (e) {
    const out = typeof e?.stdout === "string" ? e.stdout : (e?.stdout?.toString?.() ?? "");
    if (out.trim().startsWith("{")) {
      console.warn(`warning: npm ls reported problems (${String(e?.stderr ?? "").trim().split("\n")[0]}); using its output`);
      return out;
    }
    throw e;
  }
}

/** @returns {{ name: string, version: string, licence: string, texts: string[] }[]} */
export function npmLicences(appDir, ls = npmLs) {
  const root = JSON.parse(ls(appDir));
  const seen = new Map();
  const walk = (node) => {
    for (const [name, dep] of Object.entries(node.dependencies ?? {})) {
      // Not installed: its licence can't be read, so don't silently leave it out.
      if (dep.missing === true || dep.path === undefined) {
        throw new Error(`${name} is not installed in ${appDir}; run npm ci there first`);
      }
      const key = `${name}@${dep.version}`;
      if (seen.has(key)) continue;
      seen.set(key, { name, version: dep.version, path: dep.path });
      walk(dep);
    }
  };
  walk(root);
  return [...seen.values()]
    .map(({ name, version, path }) => {
      const pkg = JSON.parse(readFileSync(join(path, "package.json"), "utf8"));
      const licence = typeof pkg.license === "string" ? pkg.license : (pkg.license?.type ?? "");
      const texts = readdirSync(path)
        .filter((f) => LICENCE_FILE.test(f))
        .sort()
        .map((f) => readFileSync(join(path, f), "utf8"));
      return { name, version, licence, texts };
    })
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const appDir = fileURLToPath(new URL("..", import.meta.url));
  process.stdout.write(JSON.stringify(npmLicences(appDir), null, 2) + "\n");
}
