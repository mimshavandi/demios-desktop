// elkjs (EPL-2.0 OR GPL-3.0) must never ship (licence policy). mermaid imports it for its "elk"
// layout; the build swaps it for a stub, and `elkProblems` proves that no elkjs code got in.
// The desktop licences check (apps/desktop/licenses-extra.json, `npm_exclude`) relies on this.

/** esbuild plugin: resolve every `elkjs` import to a stub that rejects with a clear error. */
export const noElk = {
  name: "no-elkjs",
  setup(b) {
    b.onResolve({ filter: /^elkjs(\/|$)/ }, () => ({ path: "elkjs", namespace: "stub" }));
    b.onLoad({ filter: /.*/, namespace: "stub" }, () => ({
      contents:
        "export default class ELK { layout() { return Promise.reject(new Error('The ELK layout is not available in artifacts; use the default layout.')); } }",
      loader: "js",
    }));
  },
};

/** Strings found in elkjs's own code (its Java package name, compiled into the JS). */
const ELK_MARKERS = ["org.eclipse.elk", "elk-worker"];

/**
 * Problems if elkjs got into a build: an input under node_modules/elkjs in the esbuild
 * metafile, or elkjs code in the output. Empty when the build is clean.
 */
export function elkProblems(metafile, outputs) {
  const problems = [];
  for (const input of Object.keys(metafile?.inputs ?? {})) {
    if (/(^|[\\/])node_modules[\\/]elkjs[\\/]/.test(input)) problems.push(`bundled input ${input}`);
  }
  for (const [name, code] of Object.entries(outputs)) {
    for (const m of ELK_MARKERS) if (code.includes(m)) problems.push(`${name} contains "${m}"`);
  }
  return problems;
}
