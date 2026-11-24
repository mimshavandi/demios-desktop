// React artifacts: one JSX/TSX file, transpiled here with sucrase and rendered with the vendored
// React. Imports resolve only to the allow-listed modules below; there is no network.
import * as React from "react";
import * as ReactDOM from "react-dom";
import * as ReactDOMClient from "react-dom/client";
import * as JsxRuntime from "react/jsx-runtime";
import { transform } from "sucrase";
import { readSource, reportIssue, showError } from "./source";

const MODULES: Record<string, unknown> = {
  react: React,
  "react-dom": ReactDOM,
  "react-dom/client": ReactDOMClient,
  "react/jsx-runtime": JsxRuntime,
};

function requireModule(name: string): unknown {
  const m = MODULES[name];
  if (m === undefined) {
    throw new Error(
      `Cannot import "${name}": React artifacts can import only ${Object.keys(MODULES).join(", ")} (no network).`,
    );
  }
  return m;
}

interface ModuleRecord {
  exports: Record<string, unknown>;
}

class Boundary extends React.Component<{ children: React.ReactNode }, { error: Error | null }> {
  override state = { error: null as Error | null };
  static getDerivedStateFromError(error: Error): { error: Error } {
    return { error };
  }
  override componentDidCatch(error: Error, info: React.ErrorInfo): void {
    reportIssue({ kind: "render", message: `${error.name}: ${error.message}`, detail: info.componentStack ?? "" });
  }
  override render(): React.ReactNode {
    if (this.state.error) {
      return React.createElement("pre", { className: "dn-error" }, `Component crashed\n\n${this.state.error.message}`);
    }
    return this.props.children;
  }
}

function pickComponent(mod: ModuleRecord): React.ComponentType | null {
  const ex = mod.exports;
  const candidates = [ex["default"], ...Object.values(ex)];
  for (const c of candidates) {
    if (typeof c === "function") return c as React.ComponentType;
  }
  return null;
}

function run(): void {
  let code: string;
  try {
    code = transform(readSource(), {
      transforms: ["jsx", "typescript", "imports"],
      jsxRuntime: "automatic",
      production: true,
      filePath: "artifact.tsx",
    }).code;
  } catch (e) {
    showError("React artifact has a syntax error", e);
    return;
  }
  const mod: ModuleRecord = { exports: {} };
  const w = window as unknown as { __dnModule: ModuleRecord; __dnRequire: typeof requireModule };
  w.__dnModule = mod;
  w.__dnRequire = requireModule;
  // Run as an inline script (allowed by the frame CSP's 'unsafe-inline'; no eval needed).
  // Errors thrown here surface through the bridge's window "error" listener.
  const s = document.createElement("script");
  // try/catch here (not only the window "error" event): WebKit reports uncaught errors in
  // sandboxed frames as "Script error." without details.
  s.textContent = `try { (function (module, exports, require) {\n${code}\n})(window.__dnModule, window.__dnModule.exports, window.__dnRequire); } catch (e) { window.__dnModuleError = e; }`;
  document.head.appendChild(s);
  s.remove();
  const moduleError = (window as unknown as { __dnModuleError?: unknown }).__dnModuleError;
  if (moduleError !== undefined) {
    showError("React artifact failed to load", moduleError);
    return;
  }

  const Component = pickComponent(mod);
  if (!Component) {
    showError("React artifact has no component", new Error("export a component with `export default function App() { … }`"));
    return;
  }
  const root = document.getElementById("root");
  if (!root) return;
  ReactDOMClient.createRoot(root).render(
    React.createElement(Boundary, null, React.createElement(Component)),
  );
}

run();
