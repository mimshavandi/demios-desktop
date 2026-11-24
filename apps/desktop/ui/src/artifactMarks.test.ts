import { describe, expect, it } from "vitest";
import { markArtifact } from "./artifactMarks";

const T = "2026-11-24T10:00:00Z";

describe("markArtifact", () => {
  it("puts the generator meta in the HTML head", () => {
    const out = markArtifact({ kind: "html" }, "<!doctype html><html><head><title>x</title></head><body>b</body></html>", "local:dev-fake", T);
    expect(out).toMatch(/<head><meta name="generator" content="Demios \(AI-generated with local:dev-fake\)"><title>/);
  });
  it("adds a head when there is none, and handles fragments", () => {
    expect(markArtifact({ kind: "html" }, "<html><body>b</body></html>", null, T)).toMatch(/^<html><head><meta name="generator"[^>]*><\/head><body>/);
    expect(markArtifact({ kind: "html" }, "<p>hi</p>", null, T)).toMatch(/^<meta name="generator"[^>]*><p>hi<\/p>$/);
  });
  it("comments SVG after the XML declaration", () => {
    const out = markArtifact({ kind: "svg" }, '<?xml version="1.0"?><svg/>', "byok:anthropic:claude-sonnet-5-5", T);
    expect(out).toMatch(/^<\?xml version="1.0"\?><!-- generator: Demios \(AI-generated with byok:anthropic:claude-sonnet-5-5\) --><svg\/>$/);
  });
  it("prefixes Markdown with front matter", () => {
    const out = markArtifact({ kind: "markdown" }, "# Notes", "local:qwen", T);
    expect(out.startsWith("---\ngenerator: Demios\nai_generated: true\nmodel: \"local:qwen\"\ncreated: 2026-11-24T10:00:00.000Z\n---\n# Notes")).toBe(true);
  });
  it("leaves React, Mermaid and code unchanged", () => {
    for (const t of [{ kind: "react" }, { kind: "mermaid" }, { kind: "code", language: "rust" }] as const) {
      expect(markArtifact(t, "x", null, T)).toBe("x");
    }
  });
});
