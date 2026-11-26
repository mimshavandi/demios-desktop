// Skills in the real app: the skills commands must be granted to the webview (build.rs and
// capabilities/default.json). The UI unit tests use the IPC mock, so only a real app shows
// "Couldn't load the skills. list_skills not allowed".
// Opens Settings › Skills and expects every starter skill from skills/library as built in.

import { $, $$, browser } from "@wdio/globals";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const library = path.resolve(here, "../../../../skills/library");
const starters = fs
  .readdirSync(library, { withFileTypes: true })
  .filter((d) => d.isDirectory() && fs.existsSync(path.join(library, d.name, "SKILL.md")))
  .map((d) => d.name);

const byTestId = (id) => $(`[data-testid="${id}"]`);
const textOf = (el) => browser.execute((e) => e.textContent ?? "", el);
const bodyText = () => browser.execute(() => document.body.textContent ?? "");

describe("skills", () => {
  it("Settings › Skills lists the starter library", async () => {
    if (starters.length === 0) throw new Error(`no starter skills found in ${library}`);
    await (await byTestId("new-chat")).waitForExist({ timeout: 60_000 });

    await (await byTestId("open-settings")).click();
    await (await byTestId("settings-nav-skills")).click();
    const card = await byTestId("skills-card");
    await card.waitForExist({ timeout: 30_000, timeoutMsg: "the Skills page did not open" });

    await browser.waitUntil(async () => (await $$('[data-testid="skill-row"]')).length >= starters.length, {
      timeout: 30_000,
      timeoutMsg: `expected ${starters.length} skills; page says: ${(await textOf(card)).slice(0, 400)}`,
    });
    const rows = await $$('[data-testid="skill-row"]');
    const builtIn = [];
    for (const row of rows) {
      const t = await textOf(row);
      if (t.includes("Built in")) builtIn.push(t);
    }
    if (builtIn.length !== starters.length) {
      throw new Error(`expected ${starters.length} built-in skills, got ${builtIn.length}: ${builtIn.join(" | ")}`);
    }
    if ((await $$('[data-testid="skill-problems"]')).length > 0) {
      throw new Error(`skills didn't load: ${await textOf(await byTestId("skill-problems"))}`);
    }

    // No command anywhere on screen was refused (the chat's skill list loads at launch too).
    const text = await bodyText();
    if (/not allowed|Command not found|Couldn't load the skills/.test(text)) {
      throw new Error(`a command was refused: ${text.slice(0, 400)}`);
    }
  });
});
