// The main webview under Tauri's `unstable` feature (ADR-0012): with multiwebview on, the
// window's own webview becomes a child that Tauri resizes with the window, on every platform.
// It must still fill the window after resizes, and the title bar's drag region must be there.
// (Dragging the window and the macOS traffic lights are a manual check: e2e/README.md.)

import { browser, expect } from "@wdio/globals";

describe("main window under `unstable`", () => {
  it("the main webview follows the window when it is resized", async () => {
    await browser.waitUntil(async () => browser.execute(() => document.readyState === "complete"), { timeout: 60_000 });
    const size = () => browser.execute(() => [window.innerWidth, window.innerHeight]);
    await browser.setWindowSize(1100, 760);
    await browser.pause(800);
    const [w1, h1] = await size();
    await browser.setWindowSize(1400, 900);
    await browser.pause(800);
    const [w2, h2] = await size();
    console.log(
      `main webview: at 1100x760 -> ${w1}x${h1}, at 1400x900 -> ${w2}x${h2} (window rect now ${JSON.stringify(await browser.getWindowRect())}, ${await browser.execute(() => document.visibilityState)})`,
    );
    // Grows by (nearly) the same amount as the window: it fills the client area.
    expect(Math.abs(w2 - w1 - 300)).toBeLessThanOrEqual(4);
    expect(Math.abs(h2 - h1 - 140)).toBeLessThanOrEqual(4);
    expect(await browser.execute(() => document.querySelectorAll("[data-tauri-drag-region]").length)).toBeGreaterThan(0);
  });
});
