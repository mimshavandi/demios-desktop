import { useEffect, useMemo, useRef, type DragEvent, type MouseEvent } from "react";
import { renderMarkdown } from "../markdown";

// Links in model output never navigate the app window. A click asks the backend to open it
// in the system browser; it confirms with the full URL in a native dialog first (T-SHELL-09).
// Code blocks get a Copy button (design-system §4, Message).
function makeOnClick(onOpenLink: ((href: string) => void) | undefined) {
  return (e: MouseEvent<HTMLDivElement>) => {
    if (!(e.target instanceof Element)) return;
    const a = e.target.closest("a");
    if (a !== null) {
      e.preventDefault();
      // The raw attribute, not a.href: the page's base URL must not turn "/x" into a link.
      const href = a.getAttribute("href");
      if (href !== null && onOpenLink !== undefined) onOpenLink(href);
      return;
    }
    copyCode(e.target);
  };
}

// Middle-click and dragging a link would bypass the confirmation (a new window, or a drop
// into a browser): both are blocked. Ctrl/Cmd-click is an ordinary click and takes the same
// confirmed path.
function onAuxClick(e: MouseEvent<HTMLDivElement>) {
  if (e.target instanceof Element && e.target.closest("a") !== null) e.preventDefault();
}

function onDragStart(e: DragEvent<HTMLDivElement>) {
  if (e.target instanceof Element && e.target.closest("a") !== null) e.preventDefault();
}

function copyCode(target: Element) {
  const copy = target.closest<HTMLButtonElement>("button.code-copy");
  if (copy !== null) {
    const code = copy.closest("pre")?.querySelector("code")?.textContent ?? "";
    void navigator.clipboard?.writeText(code).then(() => {
      copy.textContent = "Copied";
      window.setTimeout(() => (copy.textContent = "Copy"), 1500);
    });
  }
}

export function Markdown({ source, onOpenLink }: { source: string; onOpenLink?: (href: string) => void }) {
  const html = useMemo(() => renderMarkdown(source), [source]);
  const ref = useRef<HTMLDivElement>(null);

  // Buttons are added after sanitising, so model output can never inject one.
  useEffect(() => {
    ref.current?.querySelectorAll("a").forEach((a) => a.setAttribute("draggable", "false"));
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
  const onClick = useMemo(() => makeOnClick(onOpenLink), [onOpenLink]);
  return (
    <div
      ref={ref}
      className="md"
      onClick={onClick}
      onAuxClick={onAuxClick}
      onDragStart={onDragStart}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
