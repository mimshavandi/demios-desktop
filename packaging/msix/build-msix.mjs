#!/usr/bin/env node
// Builds a Microsoft Store (MSIX) package of Demios Desktop (T-STORE-04, issue #56).
//
// Tauri 2 bundles MSI and NSIS but not MSIX, so this script:
//   1. builds the release app without an installer (`tauri build --no-bundle`),
//   2. lays out the package: the exe, the bundle resources from tauri.conf.json, tile assets
//      generated from icons/icon.png, and AppxManifest.xml rendered from AppxManifest.xml.in,
//   3. indexes the assets (makepri) and packs the package (makeappx).
// makepri and makeappx come with the Windows SDK; nothing is added to the repo's dependencies.
// The package is unsigned: the Store signs what it publishes. For sideloading, sign a copy with
// a local dev certificate (README.md).
//
// Usage (Windows, from the repo root):
//   node packaging/msix/build-msix.mjs [--store] [--skip-build] [--features a,b] [--out dir]
//     --store       refuse the placeholder identity values (a package meant for upload)
//     --skip-build  package an existing target/release build
//     --features    cargo features for the app build; a store build refuses self-update ones
//     --out         output folder (default target/msix)
// Identity values: identity.json, overridden by MSIX_IDENTITY_NAME, MSIX_PUBLISHER,
// MSIX_PUBLISHER_DISPLAY_NAME and MSIX_DISPLAY_NAME.

import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoDir = resolve(here, "..", "..");
const appDir = join(repoDir, "apps", "desktop");

/**
 * Cargo features a Store build must not contain. The Store updates the app itself, so a
 * self-updater is not allowed in the package. `updater` is the updater's cargo feature (#58);
 * apps/desktop/build.rs also refuses it when DEMIOS_DISTRIBUTION=msix.
 */
export const STORE_FORBIDDEN_FEATURES = ["updater"];

/** The app's build environment for a Store package: tells the app (and its build.rs) it is the MSIX build. */
export function storeBuildEnv(env = process.env) {
  return { ...env, DEMIOS_REQUIRE_LICENSES: "1", DEMIOS_DISTRIBUTION: "msix" };
}

/** The values identity.json ships with; `--store` refuses them. */
export const PLACEHOLDER_MARK = /placeholder|^CN=0{8}-0{4}-0{4}-0{4}-0{12}$/i;

/** `0.1.0` → `0.1.0.0`. MSIX versions are four numbers ≤ 65535; the Store needs the last one 0. */
export function msixVersion(semver) {
  const m = /^(\d+)\.(\d+)\.(\d+)(?:[-+].*)?$/.exec(String(semver));
  if (!m) throw new Error(`version "${semver}" is not major.minor.patch`);
  if (/[-+]/.test(semver)) {
    throw new Error(`version "${semver}" has a pre-release or build part, which MSIX can't express`);
  }
  const parts = m.slice(1, 4).map(Number);
  if (parts.some((p) => p > 65535)) throw new Error(`version "${semver}" has a part above 65535`);
  return [...parts, 0].join(".");
}

/** identity.json, overridden by the MSIX_* environment variables, validated. */
export function resolveIdentity(file, env = process.env, { store = false } = {}) {
  const id = {
    identityName: env.MSIX_IDENTITY_NAME || file.identityName,
    publisher: env.MSIX_PUBLISHER || file.publisher,
    publisherDisplayName: env.MSIX_PUBLISHER_DISPLAY_NAME || file.publisherDisplayName,
    displayName: env.MSIX_DISPLAY_NAME || file.displayName,
  };
  const problems = [];
  // Package/Identity/Name: 3–50 characters, letters, digits, dots and dashes.
  if (!/^[A-Za-z0-9.-]{3,50}$/.test(id.identityName ?? "")) {
    problems.push(`identity name "${id.identityName}" must be 3–50 letters, digits, dots or dashes`);
  }
  if (!/^CN=[^,]+(,.+)?$/.test(id.publisher ?? "") || id.publisher.length > 8192) {
    problems.push(`publisher "${id.publisher}" must be a distinguished name starting with CN=`);
  }
  for (const key of ["publisherDisplayName", "displayName"]) {
    if (!id[key] || id[key].length > 256) problems.push(`${key} must be 1–256 characters`);
  }
  if (store) {
    for (const key of ["identityName", "publisher"]) {
      if (PLACEHOLDER_MARK.test(id[key] ?? "")) {
        problems.push(`${key} is still the placeholder; set it from Partner Center (README.md)`);
      }
    }
  }
  if (problems.length) throw new Error(problems.join("\n"));
  return id;
}

/** Throws if a store build would contain a self-updater. */
export function checkStoreFeatures(features) {
  const bad = features.filter((f) => STORE_FORBIDDEN_FEATURES.includes(f.trim()));
  if (bad.length) {
    throw new Error(`a Store build must not self-update; drop the feature(s): ${bad.join(", ")}`);
  }
}

const xmlEscape = (s) =>
  String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&apos;");

/** Fills `{{KEY}}` from `values` (XML-escaped); fails on a missing or leftover key. */
export function renderManifest(template, values) {
  const out = template.replace(/\{\{([A-Z_]+)\}\}/g, (_, key) => {
    if (!(key in values)) throw new Error(`manifest value ${key} is missing`);
    return xmlEscape(values[key]);
  });
  // Only the comment at the top may mention the placeholder syntax.
  const body = out.replace(/<!--[\s\S]*?-->/g, "");
  if (/\{\{|\}\}/.test(body)) throw new Error("manifest still has an unfilled {{…}} value");
  return out;
}

/** The manifest values for an app config and identity. */
export function manifestValues(conf, identity, arch = "x64") {
  return {
    IDENTITY_NAME: identity.identityName,
    PUBLISHER: identity.publisher,
    PUBLISHER_DISPLAY_NAME: identity.publisherDisplayName,
    DISPLAY_NAME: identity.displayName,
    VERSION: msixVersion(conf.version),
    ARCH: arch,
    EXECUTABLE: `${conf.mainBinaryName}.exe`,
    // VisualElements/@Description allows 2048 characters; the short description fits.
    DESCRIPTION: conf.bundle.shortDescription,
  };
}

/**
 * Tile and logo images: [file, width, height, icon size as a share of the shorter side].
 * makepri indexes the scale-/targetsize- variants, so the manifest names only the base file.
 */
export const ASSETS = [
  ...[16, 24, 32, 48, 256].flatMap((s) => [
    [`Square44x44Logo.targetsize-${s}.png`, s, s, 1],
    [`Square44x44Logo.targetsize-${s}_altform-unplated.png`, s, s, 1],
  ]),
  ["Square44x44Logo.scale-100.png", 44, 44, 1],
  ["Square44x44Logo.scale-200.png", 88, 88, 1],
  ["Square150x150Logo.scale-100.png", 150, 150, 0.66],
  ["Square150x150Logo.scale-200.png", 300, 300, 0.66],
  ["Wide310x150Logo.scale-100.png", 310, 150, 0.66],
  ["Wide310x150Logo.scale-200.png", 620, 300, 0.66],
  ["StoreLogo.scale-100.png", 50, 50, 1],
  ["StoreLogo.scale-200.png", 100, 100, 1],
];

function run(cmd, args, opts = {}) {
  console.log(`> ${cmd} ${args.join(" ")}`);
  execFileSync(cmd, args, { stdio: "inherit", ...opts });
}

/** The newest Windows SDK copy of a tool (WINDOWS_SDK_BIN overrides the search). */
function sdkTool(name) {
  if (process.env.WINDOWS_SDK_BIN) return join(process.env.WINDOWS_SDK_BIN, `${name}.exe`);
  const root = "C:\\Program Files (x86)\\Windows Kits\\10\\bin";
  const versions = existsSync(root)
    ? readdirSync(root)
        .filter((v) => /^10\./.test(v) && existsSync(join(root, v, "x64", `${name}.exe`)))
        .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
    : [];
  if (!versions.length) throw new Error(`${name}.exe not found: install the Windows SDK`);
  return join(root, versions.at(-1), "x64", `${name}.exe`);
}

/** Renders ASSETS from icon.png with System.Drawing (part of Windows, no extra tool). */
function makeAssets(icon, dir) {
  mkdirSync(dir, { recursive: true });
  const specs = join(dir, "..", "assets.json");
  writeFileSync(specs, JSON.stringify(ASSETS.map(([f, w, h, k]) => ({ f: join(dir, f), w, h, k }))));
  const ps = `
Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Image]::FromFile('${icon.replace(/'/g, "''")}')
foreach ($a in (Get-Content '${specs.replace(/'/g, "''")}' -Raw | ConvertFrom-Json)) {
  $bmp = New-Object System.Drawing.Bitmap $a.w, $a.h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.Clear([System.Drawing.Color]::Transparent)
  $s = [int][Math]::Round([Math]::Min($a.w, $a.h) * $a.k)
  $g.DrawImage($src, [int](($a.w - $s) / 2), [int](($a.h - $s) / 2), $s, $s)
  $bmp.Save($a.f, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
}
$src.Dispose()`;
  run("powershell", ["-NoProfile", "-NonInteractive", "-Command", ps]);
  rmSync(specs);
}

function parseArgs(argv) {
  const a = { store: false, skipBuild: false, features: [], out: join(repoDir, "target", "msix") };
  for (let i = 0; i < argv.length; i++) {
    const v = argv[i];
    if (v === "--store") a.store = true;
    else if (v === "--skip-build") a.skipBuild = true;
    else if (v === "--features") a.features = argv[++i].split(",").filter(Boolean);
    else if (v === "--out") a.out = resolve(argv[++i]);
    else throw new Error(`unknown argument ${v}`);
  }
  return a;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  checkStoreFeatures(args.features);
  const conf = JSON.parse(readFileSync(join(appDir, "tauri.conf.json"), "utf8"));
  const identity = resolveIdentity(JSON.parse(readFileSync(join(here, "identity.json"), "utf8")), process.env, {
    store: args.store,
  });
  if (!args.store) {
    console.warn("Test package: identity values may be placeholders. Not for Store upload (use --store).");
  }
  const values = manifestValues(conf, identity);
  const manifest = renderManifest(readFileSync(join(here, "AppxManifest.xml.in"), "utf8"), values);
  if (process.platform !== "win32") {
    console.log(manifest);
    throw new Error("packing needs Windows (makeappx); the manifest above is what would be packed");
  }

  if (!args.skipBuild) {
    const build = ["tauri", "build", "--no-bundle"];
    if (args.features.length) build.push("--features", args.features.join(","));
    run("npx", build, { cwd: appDir, shell: true, env: storeBuildEnv() });
  }

  const target = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(repoDir, "target");
  const exe = join(target, "release", values.EXECUTABLE);
  if (!existsSync(exe)) throw new Error(`${exe} not found: build the release app first`);

  const layout = join(args.out, "layout");
  rmSync(layout, { recursive: true, force: true });
  mkdirSync(layout, { recursive: true });
  cpSync(exe, join(layout, values.EXECUTABLE));
  // On Windows, Tauri's resource_dir() is the exe's folder: copy bundle.resources next to it.
  for (const [from, to] of Object.entries(conf.bundle.resources ?? {})) {
    cpSync(resolve(appDir, from), join(layout, to), { recursive: true });
  }
  makeAssets(join(appDir, "icons", "icon.png"), join(layout, "Assets"));
  writeFileSync(join(layout, "AppxManifest.xml"), manifest);

  const makepri = sdkTool("makepri");
  const priconfig = join(args.out, "priconfig.xml");
  run(makepri, ["createconfig", "/cf", priconfig, "/dq", "en-US", "/o"]);
  run(makepri, ["new", "/pr", layout, "/cf", priconfig, "/of", join(layout, "resources.pri"), "/o"]);

  const msix = join(args.out, `DemiosDesktop_${values.VERSION}_${values.ARCH}.msix`);
  run(sdkTool("makeappx"), ["pack", "/d", layout, "/p", msix, "/o"]);
  console.log(`MSIX (unsigned): ${msix}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (e) {
    console.error(`build-msix: ${e.message}`);
    process.exit(1);
  }
}
