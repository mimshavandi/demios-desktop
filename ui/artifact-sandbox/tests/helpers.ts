import { expect, type FrameLocator, type Page } from "@playwright/test";
import type { ArtifactType } from "../src/types";

export const SPY = "http://127.0.0.1:4174";
export const ART = "http://localhost:4175";

export function frame(page: Page): FrameLocator {
  return page.frameLocator('[data-testid="artifact-frame"]');
}

export async function openDemo(page: Page): Promise<void> {
  await page.goto("/demo/index.html");
  await expect(page.getByTestId("op-create")).toBeVisible();
}

/** Host-side instrumentation (the test plays the app): fake Tauri IPC, storage, probe inbox. */
export async function instrumentHost(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    const calls: unknown[] = [];
    w["__ipcCalls"] = calls;
    w["__TAURI_INTERNALS__"] = { invoke: (...a: unknown[]) => calls.push(["invoke", ...a]), postMessage: (...a: unknown[]) => calls.push(["pm", ...a]) };
    w["ipc"] = { postMessage: (...a: unknown[]) => calls.push(["ipc", ...a]) };
    localStorage.setItem("dn-secret", "host-local-storage");
    sessionStorage.setItem("dn-secret", "host-session-storage");
    const probes: unknown[] = [];
    w["__probes"] = probes;
    window.addEventListener("message", (e) => {
      const d = e.data as { probe?: unknown } | null;
      if (d && typeof d === "object" && "probe" in d) probes.push(d.probe);
    });
  });
}

export async function show(page: Page, type: ArtifactType, content: string, opts: { allowNetwork?: boolean } = {}): Promise<void> {
  await page.evaluate(([t, c, o]) => window.dnDemo.show(t, c, o), [type, content, opts] as const);
}

export async function spyReset(page: Page): Promise<void> {
  await page.request.get(`${SPY}/__reset`);
}

export async function spyLog(page: Page): Promise<string[]> {
  return (await (await page.request.get(`${SPY}/__log`)).json()) as string[];
}

/** Everything the host must still have after an attack. */
export async function expectHostIntact(page: Page): Promise<void> {
  const state = await page.evaluate(() => ({
    secret: document.getElementById("host-secret")?.textContent,
    title: document.title,
    ipc: (window as unknown as { __ipcCalls: unknown[] }).__ipcCalls.length,
    ls: localStorage.getItem("dn-secret"),
    ss: sessionStorage.getItem("dn-secret"),
    lsKeys: localStorage.length,
    cookie: document.cookie,
    url: location.pathname,
    injected: document.querySelectorAll("[data-pwned]").length,
  }));
  expect(state).toEqual({
    secret: "host-secret-value",
    title: "Artifact sandbox demo",
    ipc: 0,
    ls: "host-local-storage",
    ss: "host-session-storage",
    lsKeys: 1,
    cookie: "dn_session=app-secret",
    url: "/demo/index.html",
    injected: 0,
  });
}
