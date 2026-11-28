import { describe, expect, it } from "vitest";
import { markArtifact } from "./artifactMarks";

const T = "2026-11-24T10:00:00Z";

describe("markArtifact", () => {
  it("puts the generator meta first, where the parser places it in the real head", () => {
    const doc = '<!doctype html><!-- <head> in a comment --><html><head><title>x</title></head><body>b</body></html>';
    const out = markArtifact({ kind: "html" }, doc, "local:dev-fake", T);
    expect(out.startsWith('<!doctype html><meta name="generator" content="Almend (AI-generated with local:dev-fake)">')).toBe(true);
    const parsed = new DOMParser().parseFromString(out, "text/html");
    expect(parsed.head.querySelector('meta[name="generator"]')).not.toBeNull();
    expect(parsed.title).toBe("x");
    expect(markArtifact({ kind: "html" }, "<p>hi</p>", null, T)).toMatch(/^<meta name="generator"[^>]*><p>hi<\/p>$/);
  });
  it("comments SVG after the XML declaration", () => {
    const out = markArtifact({ kind: "svg" }, '<?xml version="1.0"?><svg/>', "byok:anthropic:claude-sonnet-5-5", T);
    expect(out).toMatch(/^<\?xml version="1.0"\?><!-- generator: Almend \(AI-generated with byok:anthropic:claude-sonnet-5-5\) --><svg\/>$/);
  });
  it("prefixes Markdown with front matter", () => {
    const out = markArtifact({ kind: "markdown" }, "# Notes", "local:qwen", T);
    expect(out.startsWith("---\ngenerator: Almend\nai_generated: true\nmodel: \"local:qwen\"\ncreated: 2026-11-24T10:00:00.000Z\n---\n# Notes")).toBe(true);
  });
  it("leaves React, Mermaid and code unchanged", () => {
    for (const t of [{ kind: "react" }, { kind: "mermaid" }, { kind: "code", language: "rust" }] as const) {
      expect(markArtifact(t, "x", null, T)).toBe("x");
    }
  });
});
