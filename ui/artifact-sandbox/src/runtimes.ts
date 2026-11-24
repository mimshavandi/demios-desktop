import bridge from "runtime:bridge";
import type { RendererName, Runtimes } from "./document";

const cache = new Map<RendererName, Promise<string>>();

// Each renderer is its own chunk so the host only loads e.g. mermaid when a diagram is shown.
function load(name: RendererName): Promise<string> {
  switch (name) {
    case "markdown":
      return import("runtime:markdown").then((m) => m.default);
    case "mermaid":
      return import("runtime:mermaid").then((m) => m.default);
    case "react":
      return import("runtime:react").then((m) => m.default);
  }
}

export async function runtimesFor(name: RendererName | null): Promise<Runtimes> {
  if (!name) return { bridge };
  let p = cache.get(name);
  if (!p) {
    p = load(name);
    cache.set(name, p);
    p.catch(() => cache.delete(name));
  }
  return { bridge, renderer: await p };
}
