// Demo and test host. It plays the desktop shell: holds a store, applies model operations,
// shows the artifact panel. Tests drive it through the buttons and `window.dnDemo`.
import { ArtifactView, MemoryArtifactStore, StoreError, type Artifact, type ArtifactOp, type ArtifactType } from "../src/index";
import { SAMPLES } from "./samples";

const params = new URL(location.href).searchParams;
const frameUrl = params.get("frame") ?? "http://localhost:4175/frame/";
const CONV = "conv-demo";

const store = new MemoryArtifactStore();
const log = document.getElementById("log") as HTMLOListElement;
const panel = document.getElementById("panel") as HTMLElement;
const fixRequests: unknown[] = [];

function note(text: string): void {
  const li = document.createElement("li");
  li.textContent = text;
  log.appendChild(li);
}

const networkAllowed = new Set<string>();
const view = new ArtifactView(panel, {
  frameUrl,
  // The desktop app names the model and adds its AI-generated markers (ui/src/ai.ts) here.
  madeBy: (v) => (v.author === "model" ? "demo model" : null),
  mark: (a, v) =>
    v.author === "model" && a.artifact_type.kind === "html"
      ? v.content.replace(/<head>/i, '<head><meta name="generator" content="Demios (AI-generated with demo model)">')
      : v.content,
  allowNetwork: (id) => networkAllowed.has(id),
  onAllowNetwork: (id) => {
    networkAllowed.add(id);
    note(`network allowed for ${id}`);
  },
  onRestore: (id, n) => {
    const a = store.rollback(id, n);
    note(`rollback ${id} → v${a.versions.length} (restored v${n})`);
    void view.setArtifact(a);
  },
  onFix: (req) => {
    fixRequests.push(req);
    note(`fix request sent to model: v${req.version}, ${req.issues.length} problem(s): ${req.issues[0]?.message ?? ""}`);
  },
});

let current: Artifact | null = null;

function apply(op: ArtifactOp): Artifact | null {
  try {
    const a = store.apply(CONV, op);
    current = a;
    note(`${op.op} → ${a.id} v${a.versions.length}`);
    void view.setArtifact(a);
    return a;
  } catch (e) {
    note(`${op.op} failed: ${e instanceof StoreError ? `${e.code}: ${e.message}` : String(e)}`);
    return null;
  }
}

const PAGE_V1 = `<!doctype html>
<html><head><style>
  body { font-family: system-ui, sans-serif; margin: 0; padding: 24px; background: linear-gradient(135deg,#fdf6e3,#e8f1f8); }
  .card { background: white; border-radius: 12px; padding: 20px 24px; box-shadow: 0 2px 12px rgba(0,0,0,.08); max-width: 420px; }
  h1 { margin: 0 0 8px; font-size: 22px; color: #1d3557; }
  button { font: inherit; padding: 6px 14px; border-radius: 6px; border: 0; background: #1d3557; color: white; cursor: pointer; }
  #count { font-weight: 700; }
</style></head>
<body><div class="card">
  <h1 id="greeting">Hello from version 1</h1>
  <p>Clicked <span id="count">0</span> times.</p>
  <button id="btn" type="button">Click me</button>
</div>
<script>
  let n = 0;
  document.getElementById("btn").addEventListener("click", () => { document.getElementById("count").textContent = String(++n); });
</script></body></html>`;

function byId(id: string): HTMLElement {
  return document.querySelector(`[data-testid="${id}"]`) as HTMLElement;
}

byId("op-create").addEventListener("click", () => {
  apply({ op: "create", artifact_type: { kind: "html" }, title: "Counter card", content: PAGE_V1 });
});
byId("op-update").addEventListener("click", () => {
  if (current) apply({ op: "update", id: current.id, old_str: "Hello from version 1", new_str: "Hello from version 2" });
});
byId("op-rewrite").addEventListener("click", () => {
  if (current) apply({ op: "rewrite", id: current.id, content: PAGE_V1.replace("version 1", "a rewrite") });
});
byId("op-break").addEventListener("click", () => {
  if (current) apply({ op: "update", id: current.id, old_str: "let n = 0;", new_str: "let n = 0; undefinedFunction();" });
});
for (const b of document.querySelectorAll<HTMLButtonElement>("[data-sample]")) {
  b.addEventListener("click", () => {
    const s = SAMPLES[b.dataset["sample"] ?? ""];
    if (s) apply({ op: "create", artifact_type: s.type, title: s.title, content: s.content });
  });
}

declare global {
  interface Window {
    dnDemo: {
      store: MemoryArtifactStore;
      view: ArtifactView;
      apply: typeof apply;
      show: (type: ArtifactType, content: string, opts?: { allowNetwork?: boolean }) => Promise<void>;
      fixRequests: unknown[];
    };
  }
}

window.dnDemo = {
  store,
  view,
  apply,
  fixRequests,
  show: (type, content, opts = {}) => {
    const a = store.apply(CONV, { op: "create", artifact_type: type, title: "Test artifact", content });
    if (opts.allowNetwork) networkAllowed.add(a.id);
    current = a;
    return view.setArtifact(a);
  },
};
