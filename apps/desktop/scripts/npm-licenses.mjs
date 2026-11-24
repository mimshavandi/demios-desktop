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

/** @returns {{ name: string, version: string, licence: string, texts: string[] }[]} */
export function npmLicences(appDir) {
  const out = execFileSync("npm", ["ls", "--omit=dev", "--all", "--json", "--long"], {
    cwd: appDir,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    // On Windows `npm` is a .cmd shim.
    shell: process.platform === "win32",
  });
  const root = JSON.parse(out);
  const seen = new Map();
  const walk = (node) => {
    for (const [name, dep] of Object.entries(node.dependencies ?? {})) {
      const key = `${name}@${dep.version}`;
      if (seen.has(key) || dep.path === undefined) continue;
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
