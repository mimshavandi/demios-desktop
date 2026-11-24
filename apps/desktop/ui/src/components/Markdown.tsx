import { useEffect, useMemo, useRef, type MouseEvent } from "react";
import { renderMarkdown } from "../markdown";

// Links in model output never navigate the app window. Opening them in the system
// browser needs the opener plugin and a capability; that is a later, explicit decision.
// Code blocks get a Copy button (design-system §4, Message).
function onClick(e: MouseEvent<HTMLDivElement>) {
  if (!(e.target instanceof Element)) return;
  if (e.target.closest("a") !== null) e.preventDefault();
  const copy = e.target.closest<HTMLButtonElement>("button.code-copy");
  if (copy !== null) {
    const code = copy.closest("pre")?.querySelector("code")?.textContent ?? "";
    void navigator.clipboard?.writeText(code).then(() => {
      copy.textContent = "Copied";
      window.setTimeout(() => (copy.textContent = "Copy"), 1500);
    });
  }
}

export function Markdown({ source }: { source: string }) {
  const html = useMemo(() => renderMarkdown(source), [source]);
  const ref = useRef<HTMLDivElement>(null);

  // Buttons are added after sanitising, so model output can never inject one.
  useEffect(() => {
    ref.current?.querySelectorAll("pre.code").forEach((pre) => {
      if (pre.querySelector("button.code-copy") !== null) return;
      const b = document.createElement("button");
      b.type = "button";
      b.className = "code-copy dn-btn dn-btn-ghost dn-btn-sm";
      b.textContent = "Copy";
      b.setAttribute("aria-label", "Copy code");
      pre.prepend(b);
    });
  }, [html]);

  // `html` is sanitised by DOMPurify in renderMarkdown.
  return <div ref={ref} className="md" onClick={onClick} dangerouslySetInnerHTML={{ __html: html }} />;
}
