// elkjs (EPL-2.0 OR GPL-3.0) must never ship. The build stubs it out and checks its output
// (scripts/no-elk.mjs). This proves the check catches elkjs when the stub is missing.
import { expect, test } from "@playwright/test";
import { build } from "esbuild";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { elkProblems, noElk } from "../scripts/no-elk.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

async function bundleMermaid(withStub: boolean) {
  const r = await build({
    entryPoints: [join(root, "src", "runtime", "mermaid.ts")],
    bundle: true,
    format: "iife",
    platform: "browser",
    define: { "process.env.NODE_ENV": '"production"' },
    plugins: withStub ? [noElk] : [],
    write: false,
    metafile: true,
    logLevel: "silent",
  });
  return elkProblems(r.metafile, { mermaid: r.outputFiles[0]?.text ?? "" });
}

test.describe.configure({ mode: "serial" });

test("the elk check fails when the stub is missing, and passes with it", async ({ browserName }) => {
  test.skip(browserName !== "chromium", "bundling runs in Node; once is enough");
  test.setTimeout(120_000);
  const without = await bundleMermaid(false);
  expect(without.some((p) => p.includes("node_modules/elkjs"))).toBe(true);
  expect(without.some((p) => p.includes("org.eclipse.elk"))).toBe(true);
  expect(await bundleMermaid(true)).toEqual([]);
});
