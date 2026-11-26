// Installer smoke test (release-preview workflow): the installed release build launches and
// its main UI renders. A release build has no model, so the empty "no model" state is the
// expected screen. Run with `npm run smoke` and DEMIOS_E2E_APP set to the installed binary.

import { $, browser } from "@wdio/globals";

const byTestId = (id) => $(`[data-testid="${id}"]`);

describe("installed app", () => {
  it("launches and renders the main UI", async () => {
    await (await byTestId("new-chat")).waitForExist({
      timeout: 60_000,
      timeoutMsg: "the main UI did not render (no New chat button)",
    });
    // The empty state appears once the model catalog has loaded, which waits for the
    // keychain check (#213). On a runner with no system keychain (the Linux smoke jobs) the
    // settled state is the keychain message instead of "No model connected"; both are a
    // correctly rendered release build. "Checking your system keychain…" is not settled.
    const settled = /No model connected|No system keychain found/;
    let text = "";
    await browser.waitUntil(
      async () => {
        text = await browser.execute(() => document.body.textContent ?? "");
        return settled.test(text);
      },
      {
        timeout: 30_000,
        interval: 500,
        timeoutMsg: "the empty state did not settle",
      },
    ).catch(() => {
      throw new Error(`expected the "no model" or "no keychain" state, got: ${text.slice(0, 300)}`);
    });
    console.log(`smoke: empty state = ${text.match(settled)[0]}`);
    // No dev fake model in a release build.
    if (/Dev fake model/.test(text)) throw new Error("the dev fake model is in this build");
    console.log(`smoke: title=${JSON.stringify(await browser.getTitle())}`);
  });
});
