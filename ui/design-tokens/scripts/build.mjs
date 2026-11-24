// Generates tokens.css + tokens.ts from tokens.json and verifies WCAG contrast pairs.
// Usage: node scripts/build.mjs            (write files)
//        node scripts/build.mjs --check    (fail if files are stale or contrast fails)
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const t = JSON.parse(readFileSync(join(root, "tokens.json"), "utf8"));
const check = process.argv.includes("--check");

const lum = (hex) => {
  const n = hex.replace("#", "");
  const c = [0, 2, 4].map((i) => parseInt(n.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
};
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };

let failures = 0;
const report = [];
for (const mode of ["light", "dark"]) {
  for (const [fg, bg, min] of t.contrast) {
    const r = ratio(t.color[mode][fg], t.color[mode][bg]);
    const ok = r >= min;
    if (!ok) failures++;
    report.push(`${ok ? "ok  " : "FAIL"} ${mode.padEnd(5)} ${fg} on ${bg}: ${r.toFixed(2)} (min ${min})`);
  }
}

const vars = (obj, prefix) => Object.entries(obj).map(([k, v]) => `  --dn-${prefix}${k}: ${v};`).join("\n");
const textVars = Object.entries(t.text).map(([k, [s, l]]) => `  --dn-text-${k}: ${s};\n  --dn-leading-${k}: ${l};`).join("\n");
const common = [
  vars(t.font, "font-"), textVars, vars(t.weight, "weight-"), vars(t.space, "space-"),
  vars(t.radius, "radius-"), vars(t.motion, "motion-"), vars(t.layout, ""), vars(t.z, "z-"),
].join("\n");
const mode = (m) => `${vars(t.color[m], "color-")}\n${vars(t.shadow[m], "shadow-")}\n  color-scheme: ${m};`;

const css = `/* GENERATED from tokens.json by scripts/build.mjs — do not edit. ADR-0010. */
:root {
${common}
${mode("light")}
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
${mode("dark").replace(/^/gm, "  ")}
  }
}
:root[data-theme="dark"] {
${mode("dark")}
}
@media (prefers-reduced-motion: reduce) {
  :root { --dn-motion-fast: 0ms; --dn-motion-base: 0ms; --dn-motion-slow: 0ms; }
}
`;

const names = (obj, prefix) => Object.keys(obj).map((k) => `"${prefix}${k}"`).join(" | ");
const ts = `// GENERATED from tokens.json by scripts/build.mjs — do not edit. ADR-0010.
export type ColorToken = ${names(t.color.light, "")};
export type SpaceToken = ${names(t.space, "")};
export type RadiusToken = ${names(t.radius, "")};
export type TextToken = ${names(t.text, "")};
/** \`var(--dn-color-…)\` for a colour role. Never use raw hex in components. */
export const color = (role: ColorToken): string => \`var(--dn-color-\${role})\`;
export const space = (step: SpaceToken): string => \`var(--dn-space-\${step})\`;
export const radius = (r: RadiusToken): string => \`var(--dn-radius-\${r})\`;
`;

const out = { "tokens.css": css, "tokens.ts": ts };
let stale = false;
for (const [f, body] of Object.entries(out)) {
  const p = join(root, f);
  if (check) { if (!existsSync(p) || readFileSync(p, "utf8") !== body) { stale = true; console.error(`stale: ${f} (run node scripts/build.mjs)`); } }
  else writeFileSync(p, body);
}
console.log(report.join("\n"));
if (failures) console.error(`${failures} contrast pair(s) below minimum`);
process.exit(failures || stale ? 1 : 0);
