// T-ART-06 scenario: ask the dev fake model for an HTML artifact → it renders in the sandboxed
// frame on the `artifact` origin → "update the artifact" → v2 → restore v1 (a new v3).
// Then the in-app escape cases (threat model S6), run as artifact code inside the frame and
// judged from outside it: no IPC from the frame, no network by default, no parent DOM, and the
// frame is on the custom-scheme origin with an opaque ("null") origin.
// Runs against the dev fake model (debug build, `--features dev-fake-model`).

import { $, $$, browser, expect } from "@wdio/globals";
import http from "node:http";

const byTestId = (id) => $(`[data-testid="${id}"]`);
const textOf = (el) => browser.execute((e) => e.textContent ?? "", el);
const FRAME_ORIGIN = "artifact://localhost";
// Windows renders artifacts in a separate webview (ADR-0012), out of WebDriver's reach: the
// Windows flow below checks it through the pane and the app; artifact-child.e2e.mjs checks its
// isolation. The in-frame escape cases here are for macOS and Linux (iframe in the main webview).
const WINDOWS = process.platform === "win32";

/** e2e-only command (dev fake-model builds): the artifact webview's state. */
function childState() {
  return browser.executeAsync((done) => {
    window.__TAURI_INTERNALS__
      .invoke("artifact_child_state")
      .then(done)
      .catch((e) => done({ error: String(e) }));
  });
}

/** Waits until the pane's frame has rendered (the frame's "ready" got through and was accepted). */
async function frameReady() {
  await browser.waitUntil(
    async () => browser.execute(() => document.querySelector('[data-testid="artifact-loading"]')?.hidden === true),
    { timeout: 30_000, timeoutMsg: "the artifact never reported ready" },
  );
}

/** Conversations as the app's own window sees them, through its IPC (main frame only). */
function conversationCount() {
  return browser.executeAsync((done) => {
    window.__TAURI_INTERNALS__
      .invoke("list_conversations")
      .then((list) => done(list.length))
      .catch((e) => done(`error: ${e}`));
  });
}

/** Records every request that reaches it: the frame must never get here. */
function spyServer() {
  const hits = [];
  const server = http.createServer((req, res) => {
    hits.push(`${req.method} ${req.url}`);
    res.writeHead(200, { "content-type": "text/plain", "access-control-allow-origin": "*" });
    res.end("leaked");
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve({ server, hits, port: server.address().port })));
}

async function send(text) {
  const box = await byTestId("composer-input");
  await box.waitForEnabled({ timeout: 30_000, timeoutMsg: "composer stayed disabled (no model?)" });
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

/** Runs `fn` with the shown artifact frame as the browsing context, then switches back. */
async function inFrame(fn) {
  const frame = await $('[data-testid="artifact-pane"] [data-testid="artifact-frame"]');
  await frame.waitForExist({ timeout: 30_000, timeoutMsg: "no artifact frame in the pane" });
  await browser.switchToFrame(frame);
  try {
    return await fn();
  } finally {
    await browser.switchToParentFrame();
  }
}

/** Text of the fake artifact's heading in the frame, once it has rendered. */
async function frameHeading(expected) {
  await browser.waitUntil(
    async () => {
      try {
        return await inFrame(() =>
          browser.execute(() => document.querySelector('[data-testid="fake-artifact-heading"]')?.textContent ?? null),
        ) === expected;
      } catch {
        return false; // the frame was replaced (new version) while we looked
      }
    },
    { timeout: 30_000, timeoutMsg: `the artifact frame never showed "${expected}"` },
  );
}

async function versionLabel() {
  const select = await byTestId("version-select");
  return browser.execute((s) => s.selectedOptions[0]?.textContent ?? "", select);
}

describe("artifacts", () => {
  // Windows: on a failure, show what the artifact webview did (page loads, host requests, renders).
  afterEach(async function () {
    if (WINDOWS && this.currentTest?.state === "failed") {
      console.log("artifact webview state:", JSON.stringify(await childState(), null, 2));
    }
  });

  let spy;
  before(async () => {
    spy = await spyServer();
  });
  after(() => spy?.server.close());

  it("Windows: creates, renders (in the artifact webview), updates and restores an artifact", async function () {
    if (!WINDOWS) this.skip();
    await (await byTestId("new-chat")).waitForExist({ timeout: 60_000 });
    await (await byTestId("new-chat")).click();
    await send("Make an HTML artifact");
    await lastAnswerDone(1);
    const card = await byTestId("artifact-card");
    await card.waitForExist({ timeout: 30_000, timeoutMsg: "no artifact card in the answer" });
    expect(await textOf(card)).toContain("Fake artifact");
    await frameReady();
    expect(await versionLabel()).toMatch(/^v1 · model/);
    expect(await textOf(await byTestId("artifact-made-by"))).toMatch(/^AI-generated · Made by /);
    // No iframe in the main webview: the frame lives in the artifact webview, shown over the
    // pane's frame area.
    expect((await $$('[data-testid="artifact-frame"]')).length).toBe(0);
    const slot = await byTestId("artifact-webview-slot");
    await browser.waitUntil(async () => (await childState()).shown != null, {
      timeout: 20_000,
      timeoutMsg: "the artifact webview was never shown",
    });
    const st = await childState();
    console.log("artifact webview:", JSON.stringify(st));
    expect(st.url).toMatch(/^http:\/\/artifact\.localhost\/host\//);
    const r = await browser.execute((e) => e.getBoundingClientRect().toJSON(), slot);
    expect(Math.abs(st.shown.x - r.x)).toBeLessThan(2);
    expect(Math.abs(st.shown.y - r.y)).toBeLessThan(2);

    await send("Please update the artifact");
    await lastAnswerDone(2);
    await browser.waitUntil(async () => /^v2 · model/.test(await versionLabel()), { timeout: 20_000 });
    await frameReady();
    await (await byTestId("version-prev")).click();
    await (await byTestId("version-restore")).click();
    await browser.waitUntil(async () => /^v3 — restored from v1 · you/.test(await versionLabel()), {
      timeout: 20_000,
      timeoutMsg: "restore did not create v3",
    });
    await frameReady();
  });

  it("creates, renders, updates and restores an artifact", async function () {
    if (WINDOWS) this.skip();
    await (await byTestId("new-chat")).waitForExist({ timeout: 60_000 });
    await (await byTestId("new-chat")).click();

    // 1. Ask for an HTML artifact: a card in the answer, the pane renders v1 in the sandbox.
    await send("Make an HTML artifact");
    await lastAnswerDone(1);
    const card = await byTestId("artifact-card");
    await card.waitForExist({ timeout: 30_000, timeoutMsg: "no artifact card in the answer" });
    expect(await textOf(card)).toContain("Fake artifact");
    expect(await textOf(card)).toContain("v1");
    await frameHeading("Version one");
    expect(await versionLabel()).toMatch(/^v1 · model/);
    // AI labelling: the pane names the model, the rendered document carries the marker.
    expect(await textOf(await byTestId("artifact-made-by"))).toMatch(/^AI-generated · Made by /);

    // 2. Update → v2 (an exact-match patch through artifact_update).
    await send("Please update the artifact");
    await lastAnswerDone(2);
    await frameHeading("Version two");
    expect(await versionLabel()).toMatch(/^v2 · model/);

    // 3. Back to v1 and restore it: a new v3 by the user, with v1's content.
    await (await byTestId("version-prev")).click();
    await frameHeading("Version one");
    await (await byTestId("version-restore")).click();
    await browser.waitUntil(async () => /^v3 — restored from v1 · you/.test(await versionLabel()), {
      timeout: 20_000,
      timeoutMsg: "restore did not create v3",
    });
    await frameHeading("Version one");
  });

  it("the artifact frame can't reach IPC, the network or the app's DOM", async function () {
    if (WINDOWS) this.skip();
    const conversationsBefore = await conversationCount();
    expect(typeof conversationsBefore).toBe("number");
    const frame = await $('[data-testid="artifact-pane"] [data-testid="artifact-frame"]');
    expect(await frame.getAttribute("src")).toMatch(new RegExp(`^${FRAME_ORIGIN}/frame/`));
    expect(await frame.getAttribute("sandbox")).toBe("allow-scripts");

    const leak = `http://127.0.0.1:${spy.port}`;
    // Artifact code: each attack reports what happened. Supporting evidence only (artifact
    // code could lie); the assertions that matter are made from outside the frame below.
    const r = await inFrame(() =>
      browser.executeAsync((leakUrl, done) => {
        const out = {};
        const attempt = async (name, f) => {
          try {
            out[name] = `ok:${String(await f())}`;
          } catch (e) {
            out[name] = `blocked:${e && e.name ? e.name : String(e)}`;
          }
        };
        (async () => {
          out.origin = self.origin;
          out.href = location.href;
          out.tauri = typeof window.__TAURI_INTERNALS__;
          out.ipcGlobal = typeof window.ipc;
          out.rtc = typeof window.RTCPeerConnection;
          await attempt("parentDocument", () => parent.document.title);
          await attempt("topWrite", () => {
            top.document.body.setAttribute("data-pwned", "1");
            return "wrote";
          });
          await attempt("parentTauri", () => typeof parent.__TAURI_INTERNALS__.invoke);
          await attempt("fetchNetwork", () => fetch(`${leakUrl}/fetch`).then((x) => x.status));
          await attempt("fetchIpc", () =>
            fetch("ipc://localhost/create_conversation", { method: "POST", body: "{}" }).then((x) => x.status),
          );
          await attempt("fetchIpcHttp", () =>
            fetch("http://ipc.localhost/create_conversation", { method: "POST", body: "{}" }).then((x) => x.status),
          );
          await attempt("fetchApp", () => fetch("tauri://localhost/").then((x) => x.status));
          // WebKit exposes script message handlers to every frame; without the app's invoke
          // key Tauri must drop this (it would create a conversation if it got through).
          await attempt("rawIpcHandler", () => {
            const h = window.webkit?.messageHandlers?.ipc;
            if (!h) throw new Error("no handler");
            h.postMessage(
              JSON.stringify({ cmd: "create_conversation", callback: 1, error: 2, payload: {}, options: {}, __TAURI_INVOKE_KEY__: "guess" }),
            );
            return "posted";
          });
          await attempt("image", () => {
            const img = new Image();
            img.src = `${leakUrl}/img`;
            document.body.append(img);
            return "added";
          });
          await attempt("navigateTop", () => {
            top.location.href = `${leakUrl}/nav`;
            return "navigated";
          });
          await new Promise((res) => setTimeout(res, 1500));
          done(out);
        })();
      }, leak),
    );
    console.log("in-frame results:", JSON.stringify(r));

    // Origin: the custom scheme, opaque to the browser (sandbox without allow-same-origin).
    expect(r.origin).toBe("null");
    expect(r.href.startsWith(`${FRAME_ORIGIN}/frame/`)).toBe(true);
    // No IPC objects in the frame, and no way to the parent's.
    expect(r.tauri).toBe("undefined");
    expect(r.ipcGlobal).toBe("undefined");
    expect(r.parentTauri).toMatch(/^blocked:/);
    expect(r.parentDocument).toMatch(/^blocked:/);
    expect(r.topWrite).toMatch(/^blocked:/);
    expect(r.rtc).toBe("undefined");
    for (const k of ["fetchNetwork", "fetchIpc", "fetchIpcHttp", "fetchApp"]) {
      expect(`${k}=${r[k]}`).toMatch(/=blocked:/);
    }

    // Judged from outside the frame: nothing reached the network...
    await browser.pause(500);
    expect(spy.hits).toEqual([]);
    // ...the app's DOM and URL are untouched...
    expect(await browser.execute(() => document.querySelectorAll("[data-pwned]").length)).toBe(0);
    expect(await browser.execute(() => location.href)).not.toContain("127.0.0.1");
    // ...and no IPC call went through (the raw handler message created no conversation).
    await browser.pause(500);
    expect(await conversationCount()).toBe(conversationsBefore);
  });
});
