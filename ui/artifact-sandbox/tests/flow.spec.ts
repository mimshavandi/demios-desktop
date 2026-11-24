// Done-check: create → render → update → v2 → rollback, through the real UI.
import { expect, test } from "@playwright/test";
import { ART, frame, openDemo } from "./helpers";

test("create → render → update → v2 → rollback", async ({ page }) => {
  await openDemo(page);
  const f = frame(page);
  const select = page.getByTestId("version-select");

  // Create: v1 renders and runs its script inside the sandbox.
  await page.getByTestId("op-create").click();
  await expect(page.getByTestId("artifact-title")).toHaveText("Counter card");
  await expect(f.locator("#greeting")).toHaveText("Hello from version 1");
  await f.locator("#btn").click();
  await f.locator("#btn").click();
  await expect(f.locator("#count")).toHaveText("2");
  await expect(select.locator("option:checked")).toHaveText(/^v1 · model/);
  await expect(page.getByTestId("old-version-banner")).toBeHidden();
  // AI disclosure: the pane names the model, the rendered document carries the host's marker.
  await expect(page.getByTestId("artifact-made-by")).toHaveText("AI-generated · Made by demo model");
  await expect(f.locator('meta[name="generator"]')).toHaveAttribute("content", /AI-generated/);

  // Update (exact-match patch) → v2 is a new frame showing the new content.
  await page.getByTestId("op-update").click();
  await expect(f.locator("#greeting")).toHaveText("Hello from version 2");
  await expect(f.locator("#count")).toHaveText("0");
  await expect(select.locator("option:checked")).toHaveText(/^v2 · model/);
  await expect(page.getByTestId("version-next")).toBeDisabled();

  // A second identical patch no longer matches: store refuses, nothing changes.
  await page.getByTestId("op-update").click();
  await expect(page.getByTestId("log")).toContainText("update failed: no_match");
  await expect(select.locator("option")).toHaveCount(2);

  // Switch back to v1 with the version switcher.
  await page.getByTestId("version-prev").click();
  await expect(f.locator("#greeting")).toHaveText("Hello from version 1");
  await expect(page.getByTestId("old-version-banner")).toContainText("Viewing v1. The latest is v2.");

  // Roll back: a new v3 whose parent is v1.
  await page.getByTestId("version-restore").click();
  await expect(select.locator("option:checked")).toHaveText(/^v3 — restored from v1 · you/);
  await expect(page.getByTestId("artifact-made-by")).toBeHidden();
  await expect(f.locator("#greeting")).toHaveText("Hello from version 1");
  await expect(page.getByTestId("old-version-banner")).toBeHidden();
  const versions = await page.evaluate(() => window.dnDemo.store.list("conv-demo")[0]?.versions.map((v) => [v.n, v.parent_n, v.author]));
  expect(versions).toEqual([
    [1, null, "model"],
    [2, 1, "model"],
    [3, 1, "user"],
  ]);

  // v2 is still there and viewable.
  await select.selectOption("2");
  await expect(f.locator("#greeting")).toHaveText("Hello from version 2");
});

test("runtime errors surface with a Fix this button", async ({ page }) => {
  await openDemo(page);
  await page.getByTestId("op-create").click();
  await expect(frame(page).locator("#greeting")).toBeVisible();
  await expect(page.getByTestId("artifact-issues")).toBeHidden();
  await page.getByTestId("op-break").click();
  const issues = page.getByTestId("artifact-issues");
  await expect(issues).toBeVisible();
  // WebKit hides error details in sandboxed frames; Chromium (WebView2) gives the message.
  await expect(issues).toContainText(test.info().project.name === "webkit" ? "threw an error" : "undefinedFunction");
  await page.getByTestId("artifact-fix").click();
  await expect(page.getByTestId("artifact-fix")).toHaveText("Ask to fix");
  await expect(page.getByTestId("log")).toContainText("fix request sent to model: v2");
  const req = await page.evaluate(() => window.dnDemo.fixRequests[0] as { version: number; issues: { kind: string }[] });
  expect(req.version).toBe(2);
  expect(req.issues[0]?.kind).toBe("error");
  const prompt = await page.evaluate(() => (window.dnDemo.fixRequests[0] as { prompt: string }).prompt);
  expect(prompt).toContain("<<<ARTIFACT_REPORT");
  expect(prompt).toContain("untrusted data");
});

test("web components with shadow DOM still work (guard does not break them)", async ({ page }) => {
  await openDemo(page);
  await page.evaluate(() =>
    window.dnDemo.show(
      { kind: "html" },
      `<x-hello></x-hello><script>
        customElements.define("x-hello", class extends HTMLElement {
          connectedCallback() { const r = this.attachShadow({ mode: "open" }); r.innerHTML = "<b>from shadow</b>"; this.dataset.ok = String(this.shadowRoot === r); }
        });
      </script>`,
    ),
  );
  await expect(frame(page).locator("x-hello b")).toHaveText("from shadow");
  await expect(frame(page).locator("x-hello")).toHaveAttribute("data-ok", "true");
});

test("loading skeleton gives way to the render", async ({ page }) => {
  await openDemo(page);
  await page.getByTestId("op-create").click();
  await expect(frame(page).locator("#greeting")).toBeVisible();
  await expect(page.getByTestId("artifact-loading")).toBeHidden();
});

test("blocked network shows the notice; the per-artifact toggle re-renders with network on", async ({ page }) => {
  await openDemo(page);
  await page.evaluate(() =>
    window.dnDemo.show({ kind: "html" }, '<p id="p">hi</p><script>fetch("https://example.invalid/x").catch(() => {});</script>'),
  );
  const notice = page.getByTestId("network-blocked");
  await expect(notice).toBeVisible();
  await expect(page.getByTestId("artifact-issues")).toBeHidden();
  // Two steps: the first only explains the risk and does nothing.
  await page.getByTestId("network-allow").click();
  await expect(page.getByTestId("network-text")).toContainText("text from this chat");
  // The safe choice has focus and the confirm button starts disabled, so Enter-Enter or a
  // double-click can't enable the network.
  await expect(page.getByTestId("network-cancel")).toBeFocused();
  await expect(page.getByTestId("network-confirm")).toBeDisabled();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("network-confirm")).toBeHidden();
  await page.getByTestId("network-allow").dblclick();
  await expect(page.getByTestId("log")).not.toContainText("network allowed for");
  await expect(page.getByTestId("log")).not.toContainText("network allowed for");
  await page.getByTestId("network-cancel").click();
  await expect(page.getByTestId("network-confirm")).toBeHidden();
  await page.getByTestId("network-allow").click();
  await expect(page.getByTestId("network-confirm")).toBeEnabled();
  await page.getByTestId("network-confirm").click();
  await expect(page.getByTestId("log")).toContainText("network allowed for");
  await expect(frame(page).locator("#p")).toHaveText("hi");
  await expect(notice).toBeHidden();
});

test("frame grows to its content height", async ({ page }) => {
  await openDemo(page);
  await page.evaluate(() =>
    window.dnDemo.show({ kind: "html" }, '<div style="height:700px;background:#eee">tall</div>'),
  );
  await expect(page.getByTestId("artifact-frame")).toHaveAttribute("style", /height: 7\d\dpx/);
});

test.describe("every v1 type renders", () => {
  test("react (sucrase + vendored React, TSX, hooks)", async ({ page }) => {
    await openDemo(page);
    await page.getByRole("button", { name: "React" }).click();
    const f = frame(page);
    await expect(f.getByTestId("react-total")).toHaveText("7 check-ins");
    await f.getByRole("button", { name: "Walk S" }).first().click();
    await expect(f.getByTestId("react-total")).toHaveText("8 check-ins");
  });

  test("react import outside the allow-list is a clear error", async ({ page }) => {
    await openDemo(page);
    await page.evaluate(() =>
      window.dnDemo.show({ kind: "react" }, 'import _ from "lodash";\nexport default () => <p>{_.join(["a"])}</p>;'),
    );
    await expect(page.getByTestId("artifact-issues")).toContainText('Cannot import "lodash"');
  });

  test("react syntax error is reported", async ({ page }) => {
    await openDemo(page);
    await page.evaluate(() => window.dnDemo.show({ kind: "react" }, "export default () => <p>unclosed"));
    await expect(page.getByTestId("artifact-issues")).toContainText("syntax error");
  });

  test("mermaid", async ({ page }) => {
    await openDemo(page);
    await page.getByRole("button", { name: "Mermaid" }).click();
    await expect(frame(page).locator("#root svg")).toBeVisible({ timeout: 15_000 });
    await expect(frame(page).locator("#root svg")).toContainText("Sandbox frame");
  });

  test("mermaid syntax error is reported", async ({ page }) => {
    await openDemo(page);
    await page.evaluate(() => window.dnDemo.show({ kind: "mermaid" }, "flowchart LR\n  A -->"));
    await expect(page.getByTestId("artifact-issues")).toContainText("Mermaid", { timeout: 15_000 });
  });

  test("markdown", async ({ page }) => {
    await openDemo(page);
    await page.getByRole("button", { name: "Markdown" }).click();
    await expect(frame(page).locator("h1")).toHaveText("Release notes");
    await expect(frame(page).locator("table td").first()).toHaveText("html");
  });

  test("svg", async ({ page }) => {
    await openDemo(page);
    await page.getByRole("button", { name: "SVG" }).click();
    await expect(frame(page).locator("svg text")).toHaveText("sandboxed");
  });

  test("code is escaped, not executed", async ({ page }) => {
    await openDemo(page);
    await page.getByRole("button", { name: "Code" }).click();
    await expect(frame(page).locator(".dn-code span").first()).toHaveText("def fib(n: int) -> int:");
    await page.evaluate(() =>
      window.dnDemo.show({ kind: "code", language: "html" }, '<script>parent.postMessage({probe:"ran"},"*")</script><b>x</b>'),
    );
    await expect(frame(page).locator(".dn-code span").first()).toHaveText('<script>parent.postMessage({probe:"ran"},"*")</script><b>x</b>');
  });
});

test("markdown front matter (the host's AI marker) is not shown", async ({ page }) => {
  await openDemo(page);
  await page.evaluate(() =>
    window.dnDemo.show({ kind: "markdown" }, "---\ngenerator: Demios\nai_generated: true\n---\n---\ntitle: model_own_meta\n---\n# Notes\n\nBody text.\n"),
  );
  const f = frame(page);
  await expect(f.locator("h1")).toHaveText("Notes");
  await expect(f.locator("body")).not.toContainText("ai_generated");
  await expect(f.locator("body")).not.toContainText("model_own_meta");
});

test("the frame shell opened as a top-level page stays inert", async ({ page }) => {
  // A link or stray navigation in the app's window must not turn the shell into a renderer.
  await page.addInitScript(() => {
    const seen: unknown[] = [];
    (window as unknown as { __seen: unknown[] }).__seen = seen;
    window.addEventListener("message", (e) => seen.push(e.data));
  });
  await page.goto(`${ART}/frame/`);
  await expect(page.locator("html")).toContainText("artifact pane");
  await page.waitForTimeout(300);
  expect(await page.evaluate(() => (window as unknown as { __seen: unknown[] }).__seen)).toEqual([]);
});
