import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./markdown";

describe("renderMarkdown", () => {
  it("renders gfm basics", () => {
    const html = renderMarkdown("**bold** and `code`\n\n- a\n- b");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain("<code>code</code>");
    expect(html).toContain("<li>a</li>");
  });

  it("highlights known languages and escapes unknown ones", () => {
    const rust = renderMarkdown("```rust\nfn main() {}\n```");
    expect(rust).toContain('class="hljs language-rust"');
    expect(rust).toContain('<span class="hljs-keyword">fn</span>');
    const other = renderMarkdown("```weird\n<b>x</b>\n```");
    expect(other).toContain("&lt;b&gt;x&lt;/b&gt;");
    expect(other).not.toContain("<b>");
  });

  it("never renders raw html, scripts or javascript: links", () => {
    const html = renderMarkdown(
      '<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[x](javascript:alert(1)) <iframe src="https://e.com"></iframe>',
    );
    // Raw HTML survives only as escaped text: no real tags, no live attributes or hrefs.
    expect(html).not.toMatch(/<(script|img|iframe)/i);
    expect(html).not.toMatch(/href=/i);
    expect(html).toContain("&lt;script&gt;");
  });

  it("keeps https links", () => {
    expect(renderMarkdown("[site](https://example.com)")).toContain('href="https://example.com"');
  });
});
