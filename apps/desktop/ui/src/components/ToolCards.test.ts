import { describe, expect, it } from "vitest";
import { showArgs } from "./ToolCards";

describe("showArgs", () => {
  it("shows invisible and bidi characters as escapes", () => {
    // U+202E RIGHT-TO-LEFT OVERRIDE would make "exe.txt" look like "txt.exe".
    const out = showArgs({ path: "report\u202Etxt.exe", note: "a\u200Bb", bell: "\u0007" });
    expect(out).toContain("report\\u{202E}txt.exe");
    expect(out).toContain("a\\u{200B}b");
    expect(out).not.toMatch(/[\u202E\u200B]/);
    // JSON already escapes C0 controls; they stay visible.
    expect(out).toContain("\\u0007");
  });
});
