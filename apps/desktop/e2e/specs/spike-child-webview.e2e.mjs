// SPIKE for #75: a child webview labelled `artifact` in the main window (Tauri `unstable`
// multiwebview). The app capability names the `main` webview only, so the child should be able
// to call nothing even though Tauri injects its IPC object (and invoke key) into it.
// Assertions (agreed with the lead):
//   (a) invoke("list_conversations") from the child is refused by the ACL
//   (b) channel-data fetch with guessed ids returns nothing of main's
//   (c) event emit / listen from the child are refused
//   (d) the main webview's own IPC still works
//   (e) the child can't read the main webview's DOM or storage

import { $, browser, expect } from "@wdio/globals";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const shots = path.resolve(here, "../screenshots");

/** A screenshot of the whole screen (WebDriver's only shows the main webview). */
function osScreenshot(name) {
  fs.mkdirSync(shots, { recursive: true });
  const out = path.join(shots, `${name}-${process.platform}.png`);
  try {
    if (process.platform === "win32") {
      const ps = `Add-Type -AssemblyName System.Windows.Forms,System.Drawing; $b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; $bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height; $g=[System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size); $bmp.Save('${out.replace(/'/g, "''")}')`;
      execFileSync("powershell", ["-NoProfile", "-Command", ps], { stdio: "inherit" });
    } else if (process.platform === "linux") {
      execFileSync("import", ["-window", "root", out], { stdio: "inherit" });
    }
    console.log(`screen captured: ${out}`);
  } catch (e) {
    console.log(`could not capture the screen: ${e}`);
  }
}

function mainInvoke(cmd, args = {}) {
  return browser.executeAsync(
    (c, a, done) => {
      window.__TAURI_INTERNALS__
        .invoke(c, a)
        .then((r) => done({ ok: true, value: r }))
        .catch((e) => done({ ok: false, error: String(e) }));
    },
    cmd,
    args,
  );
}

describe("spike: artifact child webview (#75)", () => {
  it("the child webview can't call the app, read its channels or events, or see its DOM and storage", async () => {
    await (await $('[data-testid="new-chat"]')).waitForExist({ timeout: 60_000 });

    // Sentinels in the main webview the child must not see.
    await browser.execute(() => {
      localStorage.setItem("dn-spike-sentinel", "main-local");
      sessionStorage.setItem("dn-spike-sentinel", "main-session");
      const el = document.createElement("div");
      el.id = "dn-spike-sentinel";
      el.textContent = "main-dom";
      document.body.append(el);
    });
    const before = await mainInvoke("list_conversations");
    expect(before.ok).toBe(true);

    // Z-order probe: a red box in the main page exactly where the child webview goes. In the
    // screen capture, whichever is visible there is on top.
    await browser.execute(() => {
      const el = document.createElement("div");
      el.id = "dn-spike-overlay";
      el.textContent = "MAIN OVERLAY";
      el.style.cssText = "position:fixed;left:860px;top:120px;width:380px;height:260px;background:#dc2626;color:#fff;font:bold 28px sans-serif;z-index:2147483647;display:grid;place-items:center";
      document.body.append(el);
    });

    const opened = await mainInvoke("spike_open_child");
    expect(opened).toEqual({ ok: true, value: null });

    let last = null;
    try {
      await browser.waitUntil(
        async () => {
          const r = await mainInvoke("spike_child_report");
          last = r.ok ? r.value : { error: r.error };
          return last?.report != null;
        },
        { timeout: 60_000, interval: 500, timeoutMsg: "the child webview never reported" },
      );
    } finally {
      osScreenshot("spike-zorder");
      fs.mkdirSync(shots, { recursive: true });
      await browser.saveScreenshot(path.join(shots, `spike-main-webview-${process.platform}.png`));
      console.log("child stages:", JSON.stringify(last?.stages ?? last));
    }
    const r = JSON.parse(last.report);
    console.log("child webview report:", JSON.stringify(r, null, 2));

    // Context: Tauri does inject its IPC object into the child (that's the point of the test).
    console.log(`child has __TAURI_INTERNALS__: ${r.hasInternals}, ipc: ${r.hasIpc}, origin: ${r.origin}`);

    // (a) app commands are refused by the ACL
    expect(r.listConversations).toMatch(/^err:.*not allowed/s);
    expect(r.createConversation).toMatch(/^err:.*not allowed/s);
    // (b) no channel data of main's
    expect(r.channelHits).toEqual([]);
    // (c) events are refused
    expect(r.eventEmit).toMatch(/^err:.*not allowed/s);
    expect(r.eventListen).toMatch(/^err:.*not allowed/s);
    // (d) main's IPC still works, and nothing was created by the child
    const after = await mainInvoke("list_conversations");
    expect(after.ok).toBe(true);
    expect(after.value.length).toBe(before.value.length);
    // (e) main's DOM and storage are out of reach
    expect(r.storageSentinel).toBeNull();
    expect(r.sessionSentinel).toBeNull();
    expect(r.parentDom).not.toBe("visible");
    expect(r.isTop).toBe(true);
    expect(r.hasOpener).toBe(false);
    expect(r.cookie).toBe("");
  });
});

describe("spike: the main webview under `unstable` (WindowChild)", () => {
  it("follows the window when it is resized", async () => {
    const size = () => browser.execute(() => [window.innerWidth, window.innerHeight]);
    const [w0, h0] = await size();
    await browser.setWindowSize(1100, 760);
    await browser.pause(800);
    const [w1, h1] = await size();
    await browser.setWindowSize(1400, 900);
    await browser.pause(800);
    const [w2, h2] = await size();
    console.log(`main webview size: start ${w0}x${h0}, at 1100x760 -> ${w1}x${h1}, at 1400x900 -> ${w2}x${h2}`);
    expect(w2).toBeGreaterThan(w1);
    expect(h2).toBeGreaterThan(h1);
    // The title bar's drag region is still there for the OS to use.
    expect(await browser.execute(() => document.querySelectorAll("[data-tauri-drag-region]").length)).toBeGreaterThan(0);
  });
});
