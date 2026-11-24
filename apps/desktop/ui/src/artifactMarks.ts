// Machine-readable "AI-generated" marking for model-made artifacts (T-ART-06 with ui/src/ai.ts):
// the HTML generator meta, an SVG/XML comment, Markdown front matter. Applied to what the
// sandbox renders only; stored versions are never changed (exact-match updates rely on them).

import { aiFrontMatter, aiGeneratorComment, aiGeneratorMeta } from "./ai";
import type { ArtifactType } from "./ipc/types";

/** `content` with the marker for its type; React, Mermaid and code are rendered unchanged. */
export function markArtifact(type: ArtifactType, content: string, model: string | null, createdAt: string): string {
  switch (type.kind) {
    case "html":
      return markHtml(content, aiGeneratorMeta(model));
    case "svg": {
      const comment = aiGeneratorComment(model);
      // After an XML declaration (it must come first), otherwise at the start.
      const decl = /^\s*<\?xml[^>]*\?>/i.exec(content);
      return decl ? `${decl[0]}${comment}${content.slice(decl[0].length)}` : `${comment}\n${content}`;
    }
    case "markdown": {
      const ms = Date.parse(createdAt);
      return aiFrontMatter(model, Number.isNaN(ms) ? Date.now() : ms) + content;
    }
    case "react":
    case "mermaid":
    case "code":
      return content;
  }
}

function markHtml(content: string, meta: string): string {
  const head = /<head(\s[^>]*)?>/i.exec(content);
  if (head) return content.slice(0, head.index + head[0].length) + meta + content.slice(head.index + head[0].length);
  const html = /<html(\s[^>]*)?>/i.exec(content);
  if (html) return content.slice(0, html.index + html[0].length) + `<head>${meta}</head>` + content.slice(html.index + html[0].length);
  // A fragment: the parser moves a leading meta into the generated head.
  const doctype = /^\s*<!doctype[^>]*>/i.exec(content);
  return doctype ? `${doctype[0]}${meta}${content.slice(doctype[0].length)}` : `${meta}${content}`;
}
