// The main webview under Tauri's `unstable` feature (ADR-0012): with multiwebview on, the
// window's own webview becomes a child that Tauri resizes with the window, on every platform.
// It must still fill the window after resizes, and the title bar's drag region must be there.
// (Dragging the window and the macOS traffic lights are a manual check: e2e/README.md.)

import { browser, expect } from "@wdio/globals";

/** How far the webview may be from the requested size, in CSS pixels (rounding at 100 %). */
const SLACK = 4;

describe("main window under `unstable`", () => {
  it("the main webview follows the window when it is resized", async () => {
    await browser.waitUntil(async () => browser.execute(() => document.readyState === "complete"), { timeout: 60_000 });
    const inner = () => browser.execute(() => [window.innerWidth, window.innerHeight, window.devicePixelRatio]);
    const seen = [];
    // Resizes, then waits until the webview fills the new size. A fixed pause read the size
    // once and failed when the resize was slow to arrive (#416, CI run 46127907110:
    // 1100x760 read as 960x633, while the next resize landed exactly). The app places the
    // window only before showing it (window_state::open), so nothing in it resizes later;
    // a window that snapped back would still fail here, with the sizes in the message.
    const resizeTo = async (w, h) => {
      await browser.setWindowSize(w, h);
      let last = [];
      await browser.waitUntil(
        async () => {
          last = await inner();
          return Math.abs(last[0] - w) <= SLACK && Math.abs(last[1] - h) <= SLACK;
        },
        { timeout: 5_000, interval: 100 },
      ).catch(async (e) => {
        const win = await browser.getWindowSize().catch(() => null);
        throw new Error(
          `asked for ${w}x${h}: the webview is ${last[0]}x${last[1]} at ${last[2]}x, the window ${
            win ? `${win.width}x${win.height}` : "unknown"
          } (${e.message})`,
        );
      });
      seen.push(`${w}x${h} -> ${last[0]}x${last[1]}`);
      return last;
    };
    const [w1, h1] = await resizeTo(1100, 760);
    const [w2, h2] = await resizeTo(1400, 900);
    console.log(`main webview: ${seen.join(", ")}`);
    // Grows by (nearly) the same amount as the window: it fills the client area.
    expect(Math.abs(w2 - w1 - 300)).toBeLessThanOrEqual(SLACK);
    expect(Math.abs(h2 - h1 - 140)).toBeLessThanOrEqual(SLACK);
    expect(await browser.execute(() => document.querySelectorAll("[data-tauri-drag-region]").length)).toBeGreaterThan(0);
  });
});
