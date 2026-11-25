// Build: (1) in-frame runtimes as IIFE text, (2) the host library (ESM, one lazy chunk per
// renderer), (3) the demo/test page. No CDN at runtime: every library is bundled from
// node_modules at the exact versions pinned in package.json.
import { build } from "esbuild";
import { copyFile, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
// mermaid optionally uses elkjs (EPL-2.0 OR GPL-3.0): it is stubbed out, and the build fails
// if any of it gets in (runtimes and every file of dist/).
import { elkProblems, noElk } from "./no-elk.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(root, "build", "runtime");
const dist = join(root, "dist");
const RUNTIMES = { boot: "boot.ts", bridge: "bridge.ts", markdown: "markdown.ts", mermaid: "mermaid.ts", react: "react.tsx" };

await rm(join(root, "build"), { recursive: true, force: true });
await rm(dist, { recursive: true, force: true });
await mkdir(out, { recursive: true });


const sizes = {};
for (const [name, file] of Object.entries(RUNTIMES)) {
  const r = await build({
    entryPoints: [join(root, "src", "runtime", file)],
    bundle: true,
    format: "iife",
    minify: true,
    target: ["es2020", "safari15"],
    platform: "browser",
    define: { "process.env.NODE_ENV": '"production"' },
    legalComments: "eof",
    plugins: [noElk],
    write: false,
    metafile: true,
    logLevel: "warning",
  });
  const code = r.outputFiles[0].text;
  const elk = elkProblems(r.metafile, { [name]: code });
  if (elk.length > 0) throw new Error(`elkjs must not ship (EPL-2.0 OR GPL-3.0): ${elk.join("; ")}`);
  if (/<\/script/i.test(code)) throw new Error(`runtime ${name} contains </script`);
  await writeFile(join(out, `${name}.js`), code);
  sizes[name] = `${(code.length / 1024).toFixed(0)} KiB`;
}

const runtimeText = {
  name: "runtime-text",
  setup(b) {
    b.onResolve({ filter: /^runtime:/ }, (a) => ({ path: a.path.slice("runtime:".length), namespace: "runtime" }));
    b.onLoad({ filter: /.*/, namespace: "runtime" }, async (a) => ({
      contents: await readFile(join(out, `${a.path}.js`), "utf8"),
      loader: "text",
    }));
  },
};

const common = {
  bundle: true,
  format: "esm",
  splitting: true,
  target: ["es2022", "safari16"],
  platform: "browser",
  plugins: [runtimeText],
  logLevel: "warning",
  chunkNames: "chunks/[name]-[hash]",
};
await build({ ...common, entryPoints: [join(root, "src", "index.ts")], outdir: dist, minify: false });
await build({ ...common, entryPoints: [join(root, "demo", "main.ts")], outdir: join(dist, "demo"), minify: true });
await copyFile(join(root, "demo", "index.html"), join(dist, "demo", "index.html"));
await copyFile(join(root, "demo", "demo.css"), join(dist, "demo", "demo.css"));
// Design tokens (ADR-0010) for the demo host; the desktop shell loads its own copy.
for (const f of ["tokens.css", "base.css"]) {
  await copyFile(join(root, "..", "design-tokens", f), join(dist, "demo", f));
}

// Frame shell for the artifact origin. The server MUST send `csp.json`'s policy as the
// Content-Security-Policy header (`network` when the URL has ?net=1, else `strict`).
await build({
  entryPoints: [join(root, "src", "document.ts")],
  bundle: true,
  format: "esm",
  platform: "node",
  outfile: join(root, "build", "document.mjs"),
  plugins: [runtimeText],
  logLevel: "warning",
});
// A file URL, not a path: import() of "D:\\…" fails on Windows (ERR_UNSUPPORTED_ESM_URL_SCHEME).
const { artifactCsp } = await import(pathToFileURL(join(root, "build", "document.mjs")).href);
const boot = await readFile(join(out, "boot.js"), "utf8");
await mkdir(join(dist, "frame"), { recursive: true });
await writeFile(
  join(dist, "frame", "index.html"),
  `<!doctype html><html><head><meta charset="utf-8"><meta name="referrer" content="no-referrer"><script>${boot}</script></head><body></body></html>\n`,
);
await writeFile(
  join(dist, "frame", "csp.json"),
  JSON.stringify({ strict: artifactCsp(false), network: artifactCsp(true) }, null, 2) + "\n",
);
// Host page of the desktop app's artifact webview (Windows, ADR-0012). Served by the app at
// /host/ with its own CSP (apps/desktop/src/artifact_child.rs); the script is a separate file so
// that CSP needs no inline script.
await build({
  entryPoints: [join(root, "src", "host", "host.ts")],
  bundle: true,
  format: "iife",
  minify: true,
  target: ["es2020"],
  platform: "browser",
  outfile: join(dist, "host", "host.js"),
  plugins: [runtimeText],
  logLevel: "warning",
});
await writeFile(
  join(dist, "host", "index.html"),
  `<!doctype html><html><head><meta charset="utf-8"><meta name="referrer" content="no-referrer"><title>Artifact</title><script defer src="/host/host.js"></script></head><body><div id="dn-start" tabindex="0"></div><div id="dn-frame"></div><div id="dn-end" tabindex="0"></div></body></html>\n`,
);
const shipped = {};
for (const f of await readdir(dist, { recursive: true, withFileTypes: true })) {
  if (f.isFile() && /\.(js|html)$/.test(f.name)) {
    const p = join(f.parentPath, f.name);
    shipped[p.slice(root.length + 1)] = await readFile(p, "utf8");
  }
}
const leaked = elkProblems(undefined, shipped);
if (leaked.length > 0) throw new Error(`elkjs must not ship (EPL-2.0 OR GPL-3.0): ${leaked.join("; ")}`);
console.log("runtimes:", sizes);
