import { useMemo, type MouseEvent } from "react";
import { renderMarkdown } from "../markdown";

// Links in model output never navigate the app window. Opening them in the system
// browser needs the opener plugin and a capability; that is a later, explicit decision.
function blockNavigation(e: MouseEvent<HTMLDivElement>) {
  if (e.target instanceof Element && e.target.closest("a") !== null) e.preventDefault();
}

export function Markdown({ source }: { source: string }) {
  const html = useMemo(() => renderMarkdown(source), [source]);
  // `html` is sanitised by DOMPurify in renderMarkdown.
  return <div className="md" onClick={blockNavigation} dangerouslySetInnerHTML={{ __html: html }} />;
}
