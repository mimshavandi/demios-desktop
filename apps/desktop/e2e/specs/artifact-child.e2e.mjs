// Windows artifact webview (ADR-0012, threat model R8): isolation, overlays and layout.
//
// REQUIRED ON EVERY TAURI UPGRADE. The isolation rests on Tauri behaviour that its `unstable`
// multiwebview API doesn't promise to keep: a child webview whose label no capability names can
// call nothing, even though Tauri injects its IPC object (and invoke key) into it. These checks
// are the gate: an upgrade that fails them must not ship.
//
// Isolation, from the artifact webview's host page (the #86 spike's checks, now permanent):
//   (a) app commands are refused by the ACL (list/create conversations, and the child's own
//       artifact_child_open)
//   (b) channel-data fetch with guessed ids returns nothing of the main webview's
//   (c) event emit / listen are refused
//   (d) the main webview's own IPC still works, and nothing was created
//   (e) the child can't see the main webview's DOM or storage; /host/doc needs the token
// And from artifact code inside the frame (the probe artifact): the same refusals, no parent
// DOM, no host routes.
// Then: an app dialog over the pane hides the artifact webview (it would cover the dialog), and
// the webview follows the pane when the window is resized.

import { $, $$, browser, expect } from "@wdio/globals";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const WINDOWS = process.platform === "win32";
const here = path.dirname(fileURLToPath(import.meta.url));
const shots = path.resolve(here, "../screenshots");
const byTestId = (id) => $(`[data-testid="${id}"]`);

/** The whole screen (WebDriver's screenshots show the main webview only). */
function osScreenshot(name) {
  fs.mkdirSync(shots, { recursive: true });
  const out = path.join(shots, `${name}-${process.platform}.png`);
  try {
    const ps = `Add-Type -AssemblyName System.Windows.Forms,System.Drawing; $b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; $bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height; $g=[System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size); $bmp.Save('${out.replace(/'/g, "''")}')`;
    execFileSync("powershell", ["-NoProfile", "-Command", ps], { stdio: "inherit" });
    console.log(`screen captured: ${out}`);
  } catch (e) {
    console.log(`could not capture the screen: ${e}`);
  }
}

function invoke(cmd, args = {}) {
  return browser.executeAsync(
    (c, a, done) => {
      window.__TAURI_INTERNALS__
        .invoke(c, a)
        .then((value) => done({ ok: true, value }))
        .catch((e) => done({ ok: false, error: String(e) }));
    },
    cmd,
    args,
  );
}

async function childState() {
  const r = await invoke("artifact_child_state");
  if (!r.ok) throw new Error(r.error);
  return r.value;
}

/** Below 1024 CSS px the artifact pane is a full overlay over the chat (design system §3); CI
 *  runners' screens vary (#392). As a person would: hide it to reach the message box, show it
 *  again when it's needed. */
function paneIsOverlay() {
  return browser.execute(() => {
    const p = document.querySelector('[data-testid="artifact-pane"]');
    return p !== null && getComputedStyle(p).position === "fixed";
  });
}

async function setPane(open) {
  const button = await $(`button[aria-label="${open ? "Show artifacts" : "Hide artifacts"}"]`);
  if (await button.isExisting()) await button.click();
  await browser.waitUntil(async () => (await $('[data-testid="artifact-pane"]').isExisting()) === open, {
    timeout: 10_000,
    timeoutMsg: `the artifact pane didn't ${open ? "open" : "close"}`,
  });
}

async function send(text) {
  if (await paneIsOverlay()) await setPane(false);
  const box = await byTestId("composer-input");
  await box.waitForEnabled({ timeout: 30_000 });
  await box.click();
  await box.setValue(text);
  await browser.keys("Enter");
}

async function lastAnswerDone(count) {
  await browser.waitUntil(
    async () => {
      const list = await $$('[data-testid="message-assistant"]');
      if (list.length < count) return false;
      return (await list[list.length - 1].getAttribute("data-status")) === "complete";
    },
    { timeout: 60_000, timeoutMsg: `answer ${count} never completed` },
  );
}

async function shownNear(slot) {
  const st = await childState();
  if (st.shown == null) return false;
  const r = await browser.execute((e) => e.getBoundingClientRect().toJSON(), slot);
  return Math.abs(st.shown.x - r.x) < 2 && Math.abs(st.shown.y - r.y) < 2 && Math.abs(st.shown.width - r.width) < 2;
}

describe("Windows artifact webview (ADR-0012)", () => {
  before(function () {
    if (!WINDOWS) this.skip();
  });

  // On a failure, show what the artifact webview did (page loads, host requests, renders).
  afterEach(async function () {
    if (this.currentTest?.state === "failed") {
      console.log("artifact webview state:", JSON.stringify(await invoke("artifact_child_state"), null, 2));
    }
  });

  it("is isolated: the host page can call nothing and sees nothing of the main webview", async () => {
    await (await byTestId("new-chat")).waitForExist({ timeout: 60_000 });
    await browser.execute(() => {
      localStorage.setItem("dn-child-sentinel", "main-local");
      sessionStorage.setItem("dn-child-sentinel", "main-session");
      const el = document.createElement("div");
      el.id = "dn-child-sentinel";
      document.body.append(el);
    });
    await (await byTestId("new-chat")).click();
    await send("Make an HTML artifact");
    await lastAnswerDone(1);
    await browser.waitUntil(async () => (await childState()).exists, { timeout: 20_000, timeoutMsg: "no artifact webview" });
    const before = await invoke("list_conversations");
    expect(before.ok).toBe(true);

    expect((await invoke("artifact_child_probe")).ok).toBe(true);
    let report = null;
    await browser.waitUntil(
      async () => {
        const r = await invoke("artifact_child_probe_report");
        report = r.ok ? r.value : null;
        return report != null;
      },
      { timeout: 60_000, interval: 500, timeoutMsg: "the probe never reported" },
    );
    const r = JSON.parse(report);
    console.log("artifact webview probe:", JSON.stringify(r, null, 2));
    // Context: Tauri does inject its IPC object here; that's why the ACL must hold.
    expect(r.label).toBe("artifact");

    // (a) app commands are refused
    expect(r.listConversations).toMatch(/^err:.*not allowed/s);
    expect(r.createConversation).toMatch(/^err:.*not allowed/s);
    expect(r.childOpen).toMatch(/^err:.*not allowed/s);
    // (b) no channel data of the main webview's
    expect(r.channelHits).toEqual([]);
    // (c) events are refused
    expect(r.eventEmit).toMatch(/^err:.*not allowed/s);
    expect(r.eventListen).toMatch(/^err:.*not allowed/s);
    // (d) the main webview's IPC works, and nothing was created
    const after = await invoke("list_conversations");
    expect(after.ok).toBe(true);
    expect(after.value.length).toBe(before.value.length);
    // (e) the main webview's DOM and storage are out of reach; host routes need the token
    expect(r.storageSentinel).toBeNull();
    expect(r.sessionSentinel).toBeNull();
    expect(r.parentDom).not.toBe("visible");
    expect(r.isTop).toBe(true);
    expect(r.hasOpener).toBe(false);
    expect(r.cookie).toBe("");
    expect(r.hostDoc).toBe(404);
  });

  it("is isolated from inside the artifact frame too", async () => {
    const before = await invoke("list_conversations");
    await send("Make a probe artifact");
    await lastAnswerDone(2);
    await setPane(true);
    const issues = await byTestId("artifact-issues");
    await browser.waitUntil(async () => (await browser.execute((e) => e.textContent ?? "", issues)).includes("dn-probe "), {
      timeout: 60_000,
      timeoutMsg: "the probe artifact never reported",
    });
    const text = await browser.execute((e) => e.textContent ?? "", issues);
    const json = text.slice(text.indexOf("dn-probe ") + "dn-probe ".length);
    const r = JSON.parse(json.slice(0, json.lastIndexOf("}") + 1));
    console.log("artifact frame probe:", JSON.stringify(r, null, 2));
    // The frame is the opaque-origin sandbox inside the artifact webview...
    expect(r.origin).toBe("null");
    // ...where Tauri's IPC object exists (labelled as the artifact webview) but can't send: the
    // shell removed `chrome.webview` and the frame CSP blocks `ipc.localhost`, so a call either
    // never gets an answer ("timeout") or fails before it is sent. Were a call to get through,
    // the ACL refuses it ("not allowed", proven from the host page above). It must never succeed.
    expect(r.label).toBe("artifact");
    const refusedOrUnsent = /^(err:.*not allowed.*|err:.*postMessage.*|timeout)$/s;
    for (const k of ["listConversations", "createConversation"]) {
      expect(`${k}=${r[k]}`).not.toMatch(/=ok:/);
      expect(r[k]).toMatch(refusedOrUnsent);
    }
    for (const k of ["parentDocument", "topDocument", "hostDoc", "hostEvent", "ipcHttp"]) {
      expect(`${k}=${r[k]}`).toMatch(/=blocked:/);
    }
    // Judged from outside: the frame created nothing.
    const after = await invoke("list_conversations");
    expect(after.value.length).toBe(before.value.length);
  });

  it("steps aside while an app dialog is over the pane", async () => {
    const slot = await byTestId("artifact-webview-slot");
    await browser.waitUntil(() => shownNear(slot), { timeout: 20_000, timeoutMsg: "the artifact webview isn't over the pane" });
    // Settings hides the artifact pane (#174 review), so no app dialog opens over the pane from
    // the chat today. Put one there the way every app overlay is: a dialog element portalled
    // into <body>. overlay.ts finds overlays by role, so this is what any Radix dialog looks like.
    await browser.execute(() => {
      const d = document.createElement("div");
      d.setAttribute("role", "dialog");
      d.setAttribute("aria-label", "e2e overlay");
      d.id = "e2e-overlay";
      d.style.cssText = "position:fixed;inset:0;background:rgba(0,0,0,.3)";
      document.body.append(d);
    });
    await browser.waitUntil(async () => (await childState()).shown == null, {
      timeout: 5_000,
      timeoutMsg: "the artifact webview stayed over an open dialog",
    });
    osScreenshot("artifact-dialog-over-pane");
    await browser.execute(() => document.getElementById("e2e-overlay")?.remove());
    await browser.waitUntil(() => shownNear(slot), { timeout: 5_000, timeoutMsg: "the artifact webview didn't come back" });
  });

  it("follows the pane when the window is resized", async () => {
    const slot = await byTestId("artifact-webview-slot");
    for (const [w, h] of [
      [1100, 760],
      [1400, 900],
    ]) {
      await browser.setWindowSize(w, h);
      await browser.waitUntil(() => shownNear(slot), { timeout: 5_000, timeoutMsg: `not over the pane at ${w}x${h}` });
    }
    osScreenshot("artifact-after-resize");
  });
});
