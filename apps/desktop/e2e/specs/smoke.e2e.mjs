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
    const text = await browser.execute(() => document.body.textContent ?? "");
    if (!/No model connected/.test(text)) {
      throw new Error(`expected the "no model" state, got: ${text.slice(0, 300)}`);
    }
    // No dev fake model in a release build.
    if (/Dev fake model/.test(text)) throw new Error("the dev fake model is in this build");
    console.log(`smoke: title=${JSON.stringify(await browser.getTitle())}`);
  });
});
