// Design-token lint for UI code (T-DES-02, issue #53). Zero dependencies.
//
// UI code takes colours, sizes, spacing, radii, shadows and z-order from the design tokens
// (`var(--dn-…)`, ADR-0010), never raw values. This scans apps/desktop/ui/src and
// ui/artifact-sandbox/src (CSS, and CSS inside TS/TSX strings) and is a *ratchet*:
// `lint-baseline.json` records today's count per file and rule; CI fails when a count grows
// (or a new file has any), and asks to lower the baseline when a count shrinks.
//
//   node ui/design-tokens/scripts/lint-ui.mjs           check (CI)
//   node ui/design-tokens/scripts/lint-ui.mjs --update  write the baseline after fixing
//   node ui/design-tokens/scripts/lint-ui.mjs --list    print every finding

import { readdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const repo = fileURLToPath(new URL("../../..", import.meta.url));
const baselineFile = fileURLToPath(new URL("../lint-baseline.json", import.meta.url));
export const ROOTS = ["apps/desktop/ui/src", "ui/artifact-sandbox/src"];

/** One rule: a pattern over a line with comments removed. Hairlines (±1px) are allowed. */
export const RULES = {
  "raw-color": {
    why: "use a --dn-color-* token",
    test: (l) => /(?<![\w&/])#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3,4})\b(?![\w-])|\b(?:rgba?|hsla?)\(/.test(l),
  },
  "px-font": {
    why: "use a --dn-text-* token",
    test: (l) => /\bfont(?:-size)?\s*:[^;"'`]*?(?<![\w.-])\d+(?:\.\d+)?px/.test(l),
  },
  "px-spacing": {
    why: "use a --dn-space-* token",
    test: (l) => {
      const m = l.match(/\b(?:margin|padding|gap|row-gap|column-gap|inset)(?:-[a-z]+)?\s*:([^;"'`}]*)/g);
      if (m === null) return false;
      return m.some((decl) => [...decl.matchAll(/(?<![\w.-])-?(\d+(?:\.\d+)?)px/g)].some((x) => Number(x[1]) > 1));
    },
  },
  "px-radius": {
    why: "use a --dn-radius-* token",
    test: (l) => /\bborder(?:-[a-z]+)*-radius\s*:[^;"'`]*?(?<![\w.-])\d*[1-9]\d*px/.test(l),
  },
  "raw-z-index": {
    why: "use a --dn-z-* token",
    test: (l) => /\bz-index\s*:\s*-?\d+/.test(l) || /\bzIndex\s*:\s*-?\d+/.test(l),
  },
  "raw-shadow": {
    why: "use a --dn-shadow-* token",
    test: (l) =>
      [...l.matchAll(/\bbox-shadow\s*:([^;"'`}]*)/g)].some((m) => {
        const v = (m[1] ?? "").trim();
        return !/^(?:(?:none|inherit|initial|unset)\b|var\()/.test(v) && /\d/.test(v);
      }),
  },
};

/** Removes comments so notes like "issue #48" or "1px" in prose don't count. */
export function stripComments(src, isCss) {
  let out = src.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " "));
  if (!isCss) out = out.replace(/(^|[^:"'`\\])\/\/[^\n]*/g, (_, p) => p);
  return out;
}

export function lintSource(src, file) {
  const isCss = file.endsWith(".css");
  const findings = [];
  stripComments(src, isCss)
    .split("\n")
    .forEach((line, i) => {
      for (const [rule, r] of Object.entries(RULES)) {
        if (r.test(line)) findings.push({ file, line: i + 1, rule, text: line.trim().slice(0, 140) });
      }
    });
  return findings;
}

function walk(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = join(dir, e.name);
    if (e.isDirectory()) return e.name === "node_modules" || e.name === "generated" ? [] : walk(p);
    return /\.(css|tsx?)$/.test(e.name) && !/\.(test|spec)\.tsx?$/.test(e.name) && !e.name.endsWith(".d.ts") ? [p] : [];
  });
}

export function lintRepo(root = repo) {
  return ROOTS.flatMap((r) => walk(join(root, r))).flatMap((p) =>
    lintSource(readFileSync(p, "utf8"), relative(root, p).split("\\").join("/")),
  );
}

/** { "file": { "rule": count } } */
export function counts(findings) {
  const out = {};
  for (const f of findings) {
    out[f.file] ??= {};
    out[f.file][f.rule] = (out[f.file][f.rule] ?? 0) + 1;
  }
  return out;
}

/** Compares counts with the baseline: grown (fails) and shrunk (lower the baseline). */
export function compare(now, baseline) {
  const grown = [];
  const shrunk = [];
  const files = new Set([...Object.keys(now), ...Object.keys(baseline)]);
  for (const file of files) {
    const rules = new Set([...Object.keys(now[file] ?? {}), ...Object.keys(baseline[file] ?? {})]);
    for (const rule of rules) {
      const n = now[file]?.[rule] ?? 0;
      const b = baseline[file]?.[rule] ?? 0;
      if (n > b) grown.push({ file, rule, now: n, baseline: b });
      if (n < b) shrunk.push({ file, rule, now: n, baseline: b });
    }
  }
  return { grown, shrunk };
}

function main() {
  const args = new Set(process.argv.slice(2));
  const findings = lintRepo();
  const now = counts(findings);
  if (args.has("--list")) {
    for (const f of findings) console.log(`${f.file}:${f.line} ${f.rule}: ${f.text}`);
  }
  if (args.has("--update")) {
    const sorted = Object.fromEntries(Object.keys(now).sort().map((k) => [k, now[k]]));
    writeFileSync(baselineFile, JSON.stringify(sorted, null, 2) + "\n");
    console.log(`baseline: ${findings.length} known findings in ${Object.keys(now).length} files`);
    return;
  }
  const baseline = existsSync(baselineFile) ? JSON.parse(readFileSync(baselineFile, "utf8")) : {};
  const { grown, shrunk } = compare(now, baseline);
  for (const g of grown) {
    console.error(`${g.file}: ${g.rule} ${g.baseline} → ${g.now} (${RULES[g.rule].why})`);
    for (const f of findings.filter((x) => x.file === g.file && x.rule === g.rule)) {
      console.error(`    ${f.file}:${f.line}  ${f.text}`);
    }
  }
  if (grown.length > 0) {
    console.error("\nNew raw values in UI code. Use design tokens (ui/design-tokens).");
    process.exit(1);
  }
  if (shrunk.length > 0) {
    console.error(
      `Fewer raw values than the baseline in ${shrunk.map((s) => `${s.file} (${s.rule})`).join(", ")}: ` +
        "run `node ui/design-tokens/scripts/lint-ui.mjs --update` and commit the lower baseline.",
    );
    process.exit(1);
  }
  console.log(`ui lint: ${findings.length} known findings (baseline), none new`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();
