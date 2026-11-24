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
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const shots = path.resolve(here, "../screenshots");

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

    const opened = await mainInvoke("spike_open_child");
    expect(opened).toEqual({ ok: true, value: null });

    let report = null;
    await browser.waitUntil(
      async () => {
        const r = await mainInvoke("spike_child_report");
        report = r.ok ? r.value : null;
        return report !== null;
      },
      { timeout: 60_000, interval: 500, timeoutMsg: "the child webview never reported" },
    );
    const r = JSON.parse(report);
    console.log("child webview report:", JSON.stringify(r, null, 2));

    fs.mkdirSync(shots, { recursive: true });
    await browser.saveScreenshot(path.join(shots, `spike-child-webview-${process.platform}.png`));

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
