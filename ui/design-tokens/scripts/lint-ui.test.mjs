// node --test ui/design-tokens/scripts/lint-ui.test.mjs (T-DES-02)
import { test } from "node:test";
import assert from "node:assert/strict";
import { compare, counts, lintSource } from "./lint-ui.mjs";

const rules = (src, file = "a.css") => lintSource(src, file).map((f) => f.rule);

test("raw values are caught", () => {
  assert.deepEqual(rules(".a { color: #fff; }"), ["raw-color"]);
  assert.deepEqual(rules(".a { background: rgba(0,0,0,.5); }"), ["raw-color"]);
  assert.deepEqual(rules(".a { color: hsl(10 20% 30%); }"), ["raw-color"]);
  assert.deepEqual(rules(".a { font-size: 13px; }"), ["px-font"]);
  assert.deepEqual(rules(".a { font: 13px/1.5 system-ui; }"), ["px-font"]);
  assert.deepEqual(rules(".a { padding: 8px 12px; }"), ["px-spacing"]);
  assert.deepEqual(rules(".a { margin-top: -4px; }"), ["px-spacing"]);
  assert.deepEqual(rules(".a { gap: 6px; }"), ["px-spacing"]);
  assert.deepEqual(rules(".a { border-radius: 6px; }"), ["px-radius"]);
  assert.deepEqual(rules(".a { z-index: 10; }"), ["raw-z-index"]);
  assert.deepEqual(rules(".a { box-shadow: 0 1px 2px black; }"), ["raw-shadow"]);
  // CSS inside TS/TSX strings and inline styles.
  assert.deepEqual(rules('const css = `.x { color: #b42318; }`;', "a.ts"), ["raw-color"]);
  assert.deepEqual(rules('<div style={{ zIndex: 5 }} />', "a.tsx"), ["raw-z-index"]);
});

test("tokens, hairlines, comments and prose are fine", () => {
  for (const ok of [
    ".a { color: var(--dn-color-text); padding: var(--dn-space-2); }",
    ".a { box-shadow: var(--dn-shadow-2); border-radius: var(--dn-radius-lg); }",
    ".a { box-shadow: none; z-index: var(--dn-z-dialog); }",
    ".a { border: 1px solid var(--dn-color-border); margin-bottom: -1px; }",
    ".a { padding: 0; margin: 0 auto; }",
    "/* was #fff and 12px before tokens */ .a { color: var(--dn-color-text); }",
    ".a { width: calc(100% - 12px); max-width: 860px; }",
  ]) {
    assert.deepEqual(rules(ok), [], ok);
  }
  for (const ok of [
    "// Part of #48: see issue #53",
    'const href = "#anchor";',
    "const id = `local-${row.id}`;",
    '<span style={{ width: `${pct}%` }} />',
  ]) {
    assert.deepEqual(rules(ok, "a.tsx"), [], ok);
  }
});

test("the ratchet fails on growth and asks to lower the baseline on shrink", () => {
  const now = counts([
    { file: "a.css", rule: "raw-color" },
    { file: "a.css", rule: "raw-color" },
    { file: "new.css", rule: "px-font" },
  ]);
  const { grown, shrunk } = compare(now, { "a.css": { "raw-color": 1, "px-font": 2 } });
  assert.deepEqual(
    grown.map((g) => `${g.file} ${g.rule} ${g.baseline}->${g.now}`),
    ["a.css raw-color 1->2", "new.css px-font 0->1"],
  );
  assert.deepEqual(shrunk.map((s) => `${s.file} ${s.rule}`), ["a.css px-font"]);
});
