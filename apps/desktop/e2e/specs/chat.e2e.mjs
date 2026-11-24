// T-SHELL-05 scenario: launch → new chat → send → streamed reply → second send + stop →
// "Stopped" → axe check → quit → relaunch on the same data dir → history is there.
// Runs against the dev fake model (debug build, `--features dev-fake-model`).

import { $, $$, browser, expect } from "@wdio/globals";
import fs from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const axeSource = fs.readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");
const MOD = process.platform === "darwin" ? "Meta" : "Control";

const byTestId = (id) => $(`[data-testid="${id}"]`);
// textContent, not getText(): WebDriver's "visible text" rules differ between engines
// (WebKitWebDriver returned "" for rendered markdown).
const textOf = (el) => browser.execute((e) => e.textContent ?? "", el);
const allByTestId = (id) => $$(`[data-testid="${id}"]`);

async function send(text) {
  const box = await byTestId("composer-input");
  await box.waitForEnabled({ timeout: 30_000, timeoutMsg: "composer stayed disabled (no model?)" });
  await box.click();
  await box.setValue(text);
  await browser.keys("Enter");
}

async function assistant(i) {
  const list = await allByTestId("message-assistant");
  return list[i];
}

async function statusOf(i) {
  const m = await assistant(i);
  return m === undefined ? null : m.getAttribute("data-status");
}

describe("desktop chat", () => {
  it("streams, stops and keeps history across a restart", async () => {
    // Launch: the shell is up.
    await (await byTestId("new-chat")).waitForExist({ timeout: 60_000 });
    await (await byTestId("new-chat")).click();

    // 1. Send → the answer streams, then completes and quotes the prompt.
    await send("Hello from the e2e suite");
    await browser.waitUntil(async () => (await statusOf(0)) === "streaming", {
      timeout: 20_000,
      timeoutMsg: "the answer never started streaming",
    });
    await browser.waitUntil(async () => (await statusOf(0)) === "complete", {
      timeout: 60_000,
      timeoutMsg: "the answer never completed",
    });
    expect(await textOf(await assistant(0))).toContain("Hello from the e2e suite");

    // 2. Second send, stop mid-answer with Ctrl+. (⌘. on macOS) → "Stopped".
    await send("Second message, please stop me");
    await browser.waitUntil(
      async () => {
        const m = await assistant(1);
        return m !== undefined && (await textOf(m)).length > 20;
      },
      { timeout: 20_000, timeoutMsg: "the second answer never streamed" },
    );
    await browser.keys([MOD, "."]);
    await browser.waitUntil(async () => (await statusOf(1)) === "cancelled", {
      timeout: 20_000,
      timeoutMsg: "Stop did not stop the answer",
    });
    expect(await textOf(await assistant(1))).toContain("Stopped");

    // 3. Accessibility: nothing serious or critical on the chat screen.
    await browser.execute(axeSource);
    const violations = await browser.executeAsync((done) => {
      // eslint-disable-next-line no-undef
      window.axe
        .run(document, { resultTypes: ["violations"] })
        .then((r) => done(r.violations.map((v) => ({ id: v.id, impact: v.impact, nodes: v.nodes.length }))))
        .catch((e) => done([{ id: "axe-error", impact: "critical", nodes: String(e) }]));
    });
    const bad = violations.filter((v) => v.impact === "serious" || v.impact === "critical");
    expect(bad).toEqual([]);

    // 4. Quit and relaunch on the same data dir (the stopped answer is stored first).
    await browser.pause(1_500);
    await browser.reloadSession();

    await (await byTestId("conversation-item")).waitForExist({ timeout: 60_000 });
    const conversations = await allByTestId("conversation-item");
    expect(conversations.length).toBe(1);
    expect(await textOf(conversations[0])).toContain("Hello from the e2e suite");
    await conversations[0].click();

    await browser.waitUntil(async () => (await allByTestId("message-user")).length === 2, {
      timeout: 20_000,
      timeoutMsg: "the user messages did not survive the restart",
    });
    const users = await allByTestId("message-user");
    expect(await textOf(users[0])).toContain("Hello from the e2e suite");
    expect(await textOf(users[1])).toContain("Second message, please stop me");
    const answers = await allByTestId("message-assistant");
    expect(answers.length).toBe(2);
    expect(await answers[0].getAttribute("data-status")).toBe("complete");
    expect(await answers[1].getAttribute("data-status")).toBe("cancelled");
  });
});
