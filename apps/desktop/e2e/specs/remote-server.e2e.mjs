// T-PRV-05: an OpenAI-compatible server (a local mock that answers like Ollama) is added
// in Settings › Models, its model reports tool calls, chat streams from it, and a tool call
// it makes (`artifact_create`) runs and opens an artifact.

import { $, $$, browser, expect } from "@wdio/globals";
import http from "node:http";

const byTestId = (id) => $(`[data-testid="${id}"]`);
const textOf = (el) => browser.execute((e) => e.textContent ?? "", el);

/** Streams `chunks` as OpenAI chat-completion SSE events, then `[DONE]`. */
function sse(res, chunks, finish) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  const ev = (delta, finish_reason = null) =>
    `data: ${JSON.stringify({ id: "c1", object: "chat.completion.chunk", choices: [{ index: 0, delta, finish_reason }] })}\n\n`;
  let out = ev({ role: "assistant", content: "" });
  for (const c of chunks) out += ev(c);
  out += ev({}, finish);
  out += `data: ${JSON.stringify({ id: "c1", choices: [], usage: { prompt_tokens: 5, completion_tokens: 5 } })}\n\n`;
  out += "data: [DONE]\n\n";
  res.end(out);
}

/** A mock Ollama: `/v1/models`, `/api/version`, `/api/show`, streaming chat with a tool call. */
function mockServer() {
  const chats = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const json = body ? JSON.parse(body) : null;
      if (req.method === "GET" && req.url === "/v1/models") {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ object: "list", data: [{ id: "mock-chat", object: "model", owned_by: "library" }] }));
      } else if (req.method === "GET" && req.url === "/api/version") {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ version: "0.12.0" }));
      } else if (req.method === "POST" && req.url === "/api/show") {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ capabilities: ["completion", "tools"] }));
      } else if (req.method === "POST" && req.url === "/v1/chat/completions") {
        chats.push(json);
        const last = json.messages.at(-1);
        const offered = (json.tools ?? []).map((t) => t.function?.name);
        const user = [...json.messages].reverse().find((m) => m.role === "user");
        const userText = typeof user?.content === "string" ? user.content : JSON.stringify(user?.content ?? "");
        if (last.role === "tool") {
          sse(res, [{ content: "Done. " }, { content: "The artifact is open." }], "stop");
        } else if (/artifact/i.test(userText) && offered.includes("artifact_create")) {
          const args = JSON.stringify({
            type: "html",
            title: "Mock server artifact",
            content: "<!doctype html><html><body><h1>From the mock server</h1></body></html>",
          });
          sse(
            res,
            [
              { content: "Making it. " },
              { tool_calls: [{ index: 0, id: "call_mock_1", type: "function", function: { name: "artifact_create", arguments: "" } }] },
              { tool_calls: [{ index: 0, function: { arguments: args } }] },
            ],
            "tool_calls",
          );
        } else {
          sse(res, ["Hello ", "from ", "the ", "mock ", "server."].map((content) => ({ content })), "stop");
        }
      } else {
        res.writeHead(404, { "content-type": "text/plain" });
        res.end("not found");
      }
    });
  });
  return new Promise((resolve) =>
    server.listen(0, "127.0.0.1", () => resolve({ server, chats, base: `http://127.0.0.1:${server.address().port}` })),
  );
}

async function send(text) {
  const box = await byTestId("composer-input");
  await box.waitForEnabled({ timeout: 30_000, timeoutMsg: "composer stayed disabled (no model?)" });
  await box.click();
  await box.setValue(text);
  await browser.keys("Enter");
}

async function lastAnswer(count) {
  await browser.waitUntil(
    async () => {
      const list = await $$('[data-testid="message-assistant"]');
      if (list.length < count) return false;
      return (await list[list.length - 1].getAttribute("data-status")) === "complete";
    },
    { timeout: 60_000, timeoutMsg: `answer ${count} never completed` },
  );
  const list = await $$('[data-testid="message-assistant"]');
  return textOf(list[list.length - 1]);
}

describe("OpenAI-compatible server", () => {
  let mock;
  before(async () => {
    mock = await mockServer();
  });
  after(() => mock?.server.close());

  it("adds a server, chats with streaming and runs its tool calls", async () => {
    await (await byTestId("new-chat")).waitForExist({ timeout: 60_000 });

    // Settings › Models › Model servers: add the mock by address.
    // CSS plus a text match (`.x button*=Models`) is an invalid WebDriver selector: filter by text.
    let models;
    for (const b of await $$(".sidebar-foot button")) {
      if ((await textOf(b)).trim() === "Models") models = b;
    }
    if (models === undefined) throw new Error("no Models button in the sidebar");
    await models.click();
    // The form opens from "Add model server" (an empty state while there are no servers).
    const open = await byTestId("add-model-server");
    await open.waitForExist({ timeout: 30_000, timeoutMsg: "Settings › Models has no Add model server button" });
    await open.scrollIntoView();
    await open.click();
    const url = await byTestId("server-url");
    // waitForExist, not waitForDisplayed: WebKitWebDriver's visibility check fails for
    // fields below the fold of the long Models page. Scroll it in before typing.
    await url.waitForExist({
      timeout: 30_000,
      timeoutMsg: "Settings › Models has no Model servers card (server-url field)",
    });
    await url.scrollIntoView();
    await url.setValue(mock.base);
    await (await byTestId("server-name")).setValue("Mock");
    await (await byTestId("add-server")).click();
    const row = await $('[data-testid="remote-model"][data-model="mock-chat"]');
    await row.waitForExist({ timeout: 30_000, timeoutMsg: "the server's model was not listed" });
    // The server said "tools" in /api/show: no switch, a "Tool calls" badge.
    expect(await textOf(row)).toContain("Tool calls");
    // The status check lists the models again: the badge says Connected.
    const server = await byTestId("remote-server");
    await browser.waitUntil(async () => (await textOf(server)).includes("Connected"), {
      timeout: 15_000,
      timeoutMsg: "the server never showed Connected",
    });

    // Model for each task › Chat: the server's model.
    await (await $("#slot-chat")).click();
    let option;
    await browser.waitUntil(
      async () => {
        for (const o of await $$('[role="option"]')) {
          if ((await textOf(o)).includes("mock-chat")) {
            option = o;
            return true;
          }
        }
        return false;
      },
      { timeout: 10_000, timeoutMsg: "the server's model is not in the Chat picker" },
    );
    await option.click();
    await browser.waitUntil(async () => (await textOf(await $("#slot-chat"))).includes("mock-chat"), {
      timeout: 10_000,
      timeoutMsg: "the chat slot did not change to the server's model",
    });

    // Chat streams from the server.
    await (await byTestId("new-chat")).click();
    await send("Hello there");
    expect(await lastAnswer(1)).toContain("Hello from the mock server.");
    const first = mock.chats[0];
    expect(first.model).toBe("mock-chat");
    expect(first.stream).toBe(true);
    const offered = (first.tools ?? []).map((t) => t.function.name);
    expect(offered).toContain("artifact_create");

    // A tool call from the server runs: the artifact opens, and the result goes back.
    await send("Please make an artifact");
    expect(await lastAnswer(2)).toContain("The artifact is open.");
    const card = await byTestId("artifact-card");
    await card.waitForExist({ timeout: 30_000, timeoutMsg: "no artifact card for the server's tool call" });
    expect(await textOf(card)).toContain("Mock server artifact");
    const followUp = mock.chats.at(-1);
    const toolMsg = followUp.messages.find((m) => m.role === "tool");
    expect(toolMsg?.tool_call_id).toBe("call_mock_1");
  });
});
