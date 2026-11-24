import type { Metafile, Plugin } from "esbuild";
export const noElk: Plugin;
export function elkProblems(metafile: Metafile | undefined, outputs: Record<string, string>): string[];
