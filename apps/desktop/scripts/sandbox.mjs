// Builds ui/artifact-sandbox (the artifact renderer and its frame shell) for the app:
// Vite bundles its dist/index.js, and build.rs embeds dist/frame/ for the `artifact` scheme.
// Skips the build when dist is newer than every source file; `--force` always builds.
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const pkg = join(dirname(fileURLToPath(import.meta.url)), "../../../ui/artifact-sandbox");
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const run = (...args) => execFileSync(npm, args, { cwd: pkg, stdio: "inherit", shell: process.platform === "win32" });

function newest(dir) {
  let t = 0;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    t = Math.max(t, e.isDirectory() ? newest(p) : statSync(p).mtimeMs);
  }
  return t;
}

const built = join(pkg, "dist", "frame", "index.html");
const force = process.argv.includes("--force");
const inputs = Math.max(newest(join(pkg, "src")), statSync(join(pkg, "package-lock.json")).mtimeMs, statSync(join(pkg, "scripts", "build.mjs")).mtimeMs);
if (!force && existsSync(built) && statSync(built).mtimeMs >= inputs) {
  console.log("artifact sandbox is up to date");
} else {
  if (!existsSync(join(pkg, "node_modules"))) run("ci", "--no-audit", "--no-fund");
  run("run", "build");
}
