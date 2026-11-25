import { afterEach, describe, expect, it } from "vitest";
import { intersects, overlayBoxes, overlayOver, OVERLAY_SELECTOR } from "./overlay";
import { cssColorToHex } from "./artifactWebview";
import { frameKeyAction } from "./components/ArtifactPane";

function placed(html: string, rect: { left: number; top: number; width: number; height: number }): HTMLElement {
  const wrap = document.createElement("div");
  wrap.innerHTML = html;
  const el = wrap.firstElementChild as HTMLElement;
  el.getBoundingClientRect = () =>
    ({ ...rect, right: rect.left + rect.width, bottom: rect.top + rect.height, x: rect.left, y: rect.top, toJSON: () => ({}) }) as DOMRect;
  document.body.append(el);
  return el;
}

const pane = { left: 800, top: 100, right: 1200, bottom: 700 };

describe("overlay signal", () => {
  afterEach(() => document.body.replaceChildren());

  it("covers every overlay kind the app uses", () => {
    for (const html of [
      '<div class="dn-dialog-overlay"></div>',
      '<div role="alertdialog"></div>',
      '<div role="dialog"></div>',
      '<div role="menu"></div>',
      '<div class="dn-select-content" role="listbox"></div>',
      '<div class="dn-tooltip"></div>',
      '<ol class="dn-toast-viewport"><li></li></ol>',
    ]) {
      const wrap = document.createElement("div");
      wrap.innerHTML = html;
      expect(wrap.querySelector(OVERLAY_SELECTOR), html).not.toBeNull();
    }
  });

  it("is on only while an overlay is over the pane", () => {
    expect(overlayOver(pane)).toBe(false);
    placed('<div class="dn-tooltip"></div>', { left: 10, top: 10, width: 80, height: 20 });
    expect(overlayBoxes()).toHaveLength(1);
    expect(overlayOver(pane)).toBe(false); // a tooltip over the sidebar leaves the artifact alone
    const dialog = placed('<div class="dn-dialog-overlay"></div>', { left: 0, top: 0, width: 1280, height: 820 });
    expect(overlayOver(pane)).toBe(true);
    dialog.remove();
    expect(overlayOver(pane)).toBe(false);
    placed('<div role="listbox"></div>', { left: 0, top: 0, width: 0, height: 0 });
    expect(overlayOver(pane)).toBe(false); // closed (empty) parts don't count
  });

  it("intersects strictly", () => {
    expect(intersects(pane, { left: 1200, top: 100, right: 1300, bottom: 200 })).toBe(false);
    expect(intersects(pane, { left: 1199, top: 100, right: 1300, bottom: 200 })).toBe(true);
  });
});

describe("keys from the artifact frame", () => {
  const k = (key: string, mods: Partial<{ ctrl: boolean; meta: boolean; alt: boolean; shift: boolean }> = {}) => ({
    key,
    ctrl: false,
    meta: false,
    alt: false,
    shift: false,
    ...mods,
  });
  it("Escape closes the pane; only app shortcuts pass", () => {
    expect(frameKeyAction(k("Escape"), false)).toBe("close");
    expect(frameKeyAction(k("n", { ctrl: true }), false)).toBe("shortcut");
    expect(frameKeyAction(k("n", { meta: true }), true)).toBe("shortcut");
    expect(frameKeyAction(k("n", { meta: true }), false)).toBeNull();
    expect(frameKeyAction(k("w", { ctrl: true }), false)).toBeNull();
    expect(frameKeyAction(k("n", { ctrl: true, alt: true }), false)).toBeNull();
    expect(frameKeyAction(k("a"), false)).toBeNull();
  });
});

it("host page background comes from the pane's computed colour", () => {
  expect(cssColorToHex("rgb(255, 255, 255)")).toBe("#ffffff");
  expect(cssColorToHex("rgba(17, 24, 39, 1)")).toBe("#111827");
  expect(cssColorToHex("transparent")).toBe("#ffffff");
});
