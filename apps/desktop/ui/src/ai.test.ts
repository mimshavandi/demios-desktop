import { describe, expect, it } from "vitest";
import { aiFrontMatter, aiGeneratorComment, aiGeneratorMeta } from "./ai";

describe("AI-generated markers", () => {
  it("names the model in a meta tag and escapes it", () => {
    expect(aiGeneratorMeta("local:qwen")).toBe('<meta name="generator" content="Demios (AI-generated with local:qwen)">');
    expect(aiGeneratorMeta('x"><script>')).not.toContain("<script>");
    expect(aiGeneratorMeta(null)).toContain("Demios (AI-generated)");
  });

  it("keeps the XML comment well-formed", () => {
    expect(aiGeneratorComment("a--b")).toBe("<!-- generator: Demios (AI-generated with a- -b) -->");
  });

  it("writes Markdown front matter", () => {
    const fm = aiFrontMatter("byok:openai:gpt-5", Date.UTC(2026, 10, 24));
    expect(fm.split("\n")).toEqual([
      "---",
      "generator: Demios",
      "ai_generated: true",
      'model: "byok:openai:gpt-5"',
      "created: 2026-11-24T00:00:00.000Z",
      "---",
      "",
    ]);
  });
});
