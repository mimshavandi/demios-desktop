// Builds the artifact document the frame shell writes into itself. The host never parses or
// executes artifact content; it only places it into this document, escaped where it is data.

import type { ArtifactType } from "./types";

export interface DocumentOptions {
  /** Per-render token echoed by the bridge so stale frames' messages are dropped. */
  nonce: string;
  /**
   * ADR-0004 per-artifact toggle. Off by default. The enforcing policy is the frame shell's
   * response header (chosen by the server from `?net=1`); this meta copy can only narrow it.
   */
  allowNetwork?: boolean;
}

/** Inline runtime scripts, loaded lazily per artifact type (see runtimes.ts). */
export interface Runtimes {
  bridge: string;
  renderer?: string;
}

/**
 * Content-Security-Policy of every artifact document. No network: nothing may be fetched from
 * anywhere (not even the app origin); only inline script/style and data:/blob: media.
 */
export function artifactCsp(allowNetwork = false): string {
  const net = allowNetwork ? " https:" : "";
  return [
    "default-src 'none'",
    "script-src 'unsafe-inline'",
    `style-src 'unsafe-inline'${net}`,
    `img-src data: blob:${net}`,
    `font-src data:${net}`,
    `media-src data: blob:${net}`,
    `connect-src${allowNetwork ? " https:" : " 'none'"}`,
    "frame-src 'none'",
    "child-src 'none'",
    "worker-src 'none'",
    "object-src 'none'",
    "manifest-src 'none'",
    "form-action 'none'",
    "base-uri 'none'",
  ].join("; ");
}

/** JSON that is safe inside any HTML context (no `</script>`, `<!--`, or U+2028/9 surprises). */
export function jsonForHtml(value: unknown): string {
  return JSON.stringify(value)
    .replace(/</g, "\\u003c")
    .replace(/>/g, "\\u003e")
    .replace(/&/g, "\\u0026")
    .replace(/\u2028/g, "\\u2028")
    .replace(/\u2029/g, "\\u2029");
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

/** Inline `<script>` text must not contain `</script` (built runtimes are checked at build time). */
function inlineScript(code: string, attrs = ""): string {
  if (/<\/script/i.test(code)) throw new Error("runtime contains </script>");
  return `<script${attrs}>${code}</script>`;
}

// Artifact content gets no app CSS (ADR-0010): html, svg and react documents only get the error
// box style. RENDERED_CSS applies only to output our own renderers produce (markdown, code, mermaid).
const ERROR_CSS = `
.dn-error { margin: 0; padding: 16px; white-space: pre-wrap; font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; color: #b42318; background: #fef3f2; }
`;

const RENDERED_CSS = `
:root { color-scheme: light dark; }
body { margin: 0; background: Canvas; color: CanvasText; font: 15px/1.55 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
`;

const DOC_CSS = `
body { padding: 16px 20px; max-width: 860px; }
pre, code { font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }
pre { overflow: auto; padding: 12px; border-radius: 6px; background: rgba(127,127,127,.12); }
table { border-collapse: collapse; } th, td { border: 1px solid rgba(127,127,127,.4); padding: 4px 8px; }
img { max-width: 100%; }
`;

const CODE_CSS = `
body { padding: 0; }
.dn-code { margin: 0; padding: 12px 0; counter-reset: line; font: 13px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace; tab-size: 4; }
.dn-code span { display: block; padding: 0 16px 0 0; white-space: pre; }
.dn-code span::before { counter-increment: line; content: counter(line); display: inline-block; width: 3.5em; padding-right: 1em; text-align: right; color: rgba(127,127,127,.8); user-select: none; }
`;

const CENTER_CSS = `body { display: flex; justify-content: center; align-items: flex-start; padding: 16px; } body > svg { max-width: 100%; height: auto; }`;

/**
 * Declarative shadow DOM (`<template shadowrootmode>`) builds closed shadow roots that the
 * frame's nested-frame guard cannot see into. Artifacts don't need it; rename the attribute.
 */
export function neutraliseDeclarativeShadow(html: string): string {
  return html.replace(/shadowrootmode/gi, "data-dn-blocked");
}

/** Which inline renderer (if any) a type needs. */
export type RendererName = "markdown" | "mermaid" | "react";

export function rendererFor(t: ArtifactType): RendererName | null {
  switch (t.kind) {
    case "markdown":
      return "markdown";
    case "mermaid":
      return "mermaid";
    case "react":
      return "react";
    default:
      return null;
  }
}

export function buildDocument(type: ArtifactType, content: string, rt: Runtimes, opts: DocumentOptions): string {
  const head = [
    "<!doctype html>",
    '<html><head><meta charset="utf-8">',
    `<meta http-equiv="Content-Security-Policy" content="${escapeHtml(artifactCsp(opts.allowNetwork ?? false))}">`,
    '<meta name="referrer" content="no-referrer">',
    '<meta name="viewport" content="width=device-width, initial-scale=1">',
    `<style>${ERROR_CSS}</style>`,
    inlineScript(rt.bridge, ` data-nonce="${escapeHtml(opts.nonce)}"`),
  ].join("");

  const data = (): string => `<script type="application/json" id="dn-src">${jsonForHtml(content)}</script>`;
  const renderer = (): string => {
    if (!rt.renderer) throw new Error(`missing renderer for ${type.kind}`);
    return inlineScript(rt.renderer);
  };

  switch (type.kind) {
    case "html":
      // The artifact's own document follows our head; the parser folds its <html>/<head> into ours.
      return `${head}${neutraliseDeclarativeShadow(content)}`;
    case "svg":
      return `${head}<style>${CENTER_CSS}</style></head><body>${neutraliseDeclarativeShadow(content)}</body></html>`;
    case "code": {
      const lines = content.replace(/\r\n?/g, "\n").split("\n");
      const body = lines.map((l) => `<span>${escapeHtml(l)}</span>`).join("");
      return `${head}<style>${RENDERED_CSS}${CODE_CSS}</style></head><body><pre class="dn-code" data-language="${escapeHtml(type.language)}">${body}</pre></body></html>`;
    }
    case "markdown":
      return `${head}<style>${RENDERED_CSS}${DOC_CSS}</style></head><body><article id="root"></article>${data()}${renderer()}</body></html>`;
    case "mermaid":
      return `${head}<style>${RENDERED_CSS}${CENTER_CSS}</style></head><body><div id="root"></div>${data()}${renderer()}</body></html>`;
    case "react":
      return `${head}</head><body><div id="root"></div>${data()}${renderer()}</body></html>`;
  }
}
