// SPIKE for #75: with Tauri `unstable` on (every window's webview becomes an auto-resizing
// child, WindowChild) but NO artifact webview added, the main webview must still fill the
// window. Runs in its own app session, before any child webview exists.

import { browser, expect } from "@wdio/globals";

describe("spike: `unstable` with no artifact webview", () => {
  it("the main webview fills the window after resizes", async () => {
    await browser.waitUntil(() => browser.execute(() => document.readyState === "complete"), { timeout: 60_000 });
    const inner = () => browser.execute(() => [window.innerWidth, window.innerHeight]);
    await browser.setWindowSize(1100, 760);
    await browser.pause(800);
    const [w1, h1] = await inner();
    await browser.setWindowSize(1400, 900);
    await browser.pause(800);
    const [w2, h2] = await inner();
    console.log(`no child: at 1100x760 -> ${w1}x${h1}, at 1400x900 -> ${w2}x${h2}`);
    // The webview grows by exactly what the window grows (decorations are a constant).
    expect(Math.abs(w2 - w1 - 300)).toBeLessThanOrEqual(4);
    expect(Math.abs(h2 - h1 - 140)).toBeLessThanOrEqual(4);
    // And it fills the window's height, not a fraction of it (title bar/borders < 120 px).
    expect(h2).toBeGreaterThan(900 - 120);
  });
});
