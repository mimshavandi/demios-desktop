import { describe, expect, it } from "vitest";
import { withoutArtifactBlocks } from "./fenced";

describe("withoutArtifactBlocks", () => {
  it("removes complete blocks and keeps the prose", () => {
    const t = 'Here it is:\n<artifact id="p" type="html" title="Page">\n<p>x</p>\n</artifact>\nDone.';
    expect(withoutArtifactBlocks(t)).toBe("Here it is:\nDone.");
  });
  it("keeps an unterminated block (it was not saved)", () => {
    const t = 'Start\n<artifact id="p" type="html" title="Page">\n<p>x</p>';
    expect(withoutArtifactBlocks(t)).toBe(t);
  });
  it("ignores tags that are not at line start", () => {
    const t = 'Use <artifact id="x"> tags like this </artifact> inline.';
    expect(withoutArtifactBlocks(t)).toBe(t);
  });
});
