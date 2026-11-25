// The one place that knows whether an app overlay is open: a dialog, menu, select list, toast or
// tooltip. On Windows the artifact renders in a native child webview that is drawn above the
// whole main page (ADR-0012), so it has to step aside while an overlay would be under it. Every
// overlay in the app is a Radix part (or its overlay) portalled into <body>, so one observer and
// one selector cover them all: no component has to report itself, and a new dialog or menu is
// covered without extra code.

/** Elements that are app overlays while they are in the document. */
export const OVERLAY_SELECTOR = [
  ".dn-dialog-overlay",
  "[role=dialog]",
  "[role=alertdialog]",
  "[role=menu]",
  "[role=listbox]",
  ".dn-tooltip",
  "[role=tooltip]",
  ".dn-select-content",
  ".dn-toast-viewport > li",
  "[data-radix-popper-content-wrapper]",
].join(",");

export interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** The on-screen boxes of the open overlays (empty ones are ignored). */
export function overlayBoxes(root: ParentNode = document): Box[] {
  const out: Box[] = [];
  for (const el of root.querySelectorAll(OVERLAY_SELECTOR)) {
    const r = el.getBoundingClientRect();
    if (r.width > 0 && r.height > 0) out.push({ left: r.left, top: r.top, right: r.right, bottom: r.bottom });
  }
  return out;
}

export function intersects(a: Box, b: Box): boolean {
  return a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
}

/** Whether any open overlay covers part of `box`. */
export function overlayOver(box: Box, root: ParentNode = document): boolean {
  return overlayBoxes(root).some((o) => intersects(o, box));
}

/**
 * Calls `onChange` (at most once per animation frame) whenever the page may have changed in a
 * way that moves the pane or opens or closes an overlay: DOM changes anywhere (overlays mount,
 * the sidebar or pane toggles), window resizes and scrolling in any scroll container.
 */
export function watchLayout(onChange: () => void): () => void {
  let frame = 0;
  const schedule = (): void => {
    if (frame !== 0) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      onChange();
    });
  };
  const mo = new MutationObserver(schedule);
  mo.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["class", "style", "hidden", "data-state", "open"] });
  window.addEventListener("resize", schedule);
  document.addEventListener("scroll", schedule, true);
  // Overlay enter and exit animations change position without a DOM change.
  document.addEventListener("animationend", schedule, true);
  document.addEventListener("transitionend", schedule, true);
  schedule();
  return () => {
    mo.disconnect();
    window.removeEventListener("resize", schedule);
    document.removeEventListener("scroll", schedule, true);
    document.removeEventListener("animationend", schedule, true);
    document.removeEventListener("transitionend", schedule, true);
    if (frame !== 0) cancelAnimationFrame(frame);
  };
}
