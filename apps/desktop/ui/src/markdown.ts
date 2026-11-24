// Markdown → sanitised HTML for chat messages. Model output is untrusted: everything goes
// through DOMPurify, raw HTML in the markdown is escaped (not rendered), and code blocks
// are highlighted with highlight.js. Rich, executable content belongs in artifacts.

import DOMPurify from "dompurify";
import { Marked, type Tokens } from "marked";
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import go from "highlight.js/lib/languages/go";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

const languages = {
  bash,
  css,
  go,
  java,
  javascript,
  json,
  markdown,
  python,
  rust,
  sql,
  typescript,
  xml,
  yaml,
};
for (const [name, lang] of Object.entries(languages)) hljs.registerLanguage(name, lang);
hljs.registerAliases(["sh", "shell", "zsh"], { languageName: "bash" });
hljs.registerAliases(["js", "jsx"], { languageName: "javascript" });
hljs.registerAliases(["ts", "tsx"], { languageName: "typescript" });
hljs.registerAliases(["html", "svg"], { languageName: "xml" });
hljs.registerAliases(["py"], { languageName: "python" });
hljs.registerAliases(["rs"], { languageName: "rust" });
hljs.registerAliases(["yml"], { languageName: "yaml" });

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

const marked = new Marked({
  gfm: true,
  breaks: false,
  renderer: {
    code({ text, lang }: Tokens.Code): string {
      const name = (lang ?? "").trim().split(/\s+/)[0]?.toLowerCase() ?? "";
      const known = name !== "" && hljs.getLanguage(name) !== undefined;
      const body = known
        ? hljs.highlight(text, { language: name, ignoreIllegals: true }).value
        : escapeHtml(text);
      const label = name === "" ? "" : `<span class="code-lang">${escapeHtml(name)}</span>`;
      return `<pre class="code">${label}<code class="hljs${known ? ` language-${name}` : ""}">${body}</code></pre>`;
    },
    // Raw HTML from the model is shown as text, never parsed.
    html({ text }: Tokens.HTML | Tokens.Tag): string {
      return escapeHtml(text);
    },
  },
});

const purifyConfig = {
  ALLOWED_TAGS: [
    "a", "b", "blockquote", "br", "code", "del", "em", "h1", "h2", "h3", "h4", "h5", "h6",
    "hr", "i", "input", "li", "ol", "p", "pre", "span", "strong", "table", "tbody", "td",
    "th", "thead", "tr", "ul",
  ],
  ALLOWED_ATTR: ["href", "title", "class", "type", "checked", "disabled", "align"],
  ALLOWED_URI_REGEXP: /^(?:https?|mailto):/i,
};

// Only our own highlighting classes survive; model output cannot borrow app styles.
const CLASS_OK = /^(?:hljs(?:-[\w-]+)?|language-[\w-]+|code|code-lang)$/;
DOMPurify.addHook("uponSanitizeAttribute", (_node, data) => {
  if (data.attrName === "class") {
    const kept = data.attrValue.split(/\s+/).filter((c) => CLASS_OK.test(c));
    if (kept.length === 0) data.keepAttr = false;
    else data.attrValue = kept.join(" ");
  }
});

export function renderMarkdown(src: string): string {
  const html = marked.parse(src, { async: false });
  return DOMPurify.sanitize(html, purifyConfig);
}
