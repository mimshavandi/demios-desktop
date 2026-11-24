// Shared helpers for renderers running inside the frame.
import type { SandboxIssue } from "../types";

/** The artifact source, embedded by the host as `<script type="application/json" id="dn-src">`. */
export function readSource(): string {
  const el = document.getElementById("dn-src");
  const parsed: unknown = JSON.parse(el?.textContent ?? '""');
  return typeof parsed === "string" ? parsed : "";
}

export function reportIssue(issue: SandboxIssue): void {
  const hook = (window as unknown as { __dnIssue?: (i: SandboxIssue) => void }).__dnIssue;
  hook?.(issue);
}

export function showError(title: string, err: unknown): void {
  const message = err instanceof Error ? err.message : String(err);
  reportIssue({ kind: "render", message: `${title}: ${message}`, detail: err instanceof Error ? (err.stack ?? "") : "" });
  const box = document.createElement("pre");
  box.className = "dn-error";
  box.textContent = `${title}\n\n${message}`;
  document.body.replaceChildren(box);
}
