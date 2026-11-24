import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { McpServerToolsView, McpToolView, ToolChoice } from "../ipc/types";
import { McpToolsPanel } from "./McpTools";
import { TooltipProvider } from "./primitives";
import { ToastProvider } from "./toast";

afterEach(cleanup);

globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;
// Radix Select calls these on open; jsdom lacks them.
Element.prototype.hasPointerCapture ??= () => false;
Element.prototype.releasePointerCapture ??= () => {};
Element.prototype.scrollIntoView ??= () => {};

const tool = (name: string, patch: Partial<McpToolView> = {}): McpToolView => ({
  id: `mcp.files.${name}`,
  name,
  title: null,
  description: "",
  seeded_risk: "read",
  approval: "auto",
  runs_without_asking: true,
  disabled: false,
  choice: "default",
  flag: null,
  fingerprint: `fp-${name}`,
  ...patch,
});

function fake(tools: McpToolView[]) {
  let view: McpServerToolsView = { server_id: "files", connected: true, tools };
  const calls: [string, unknown][] = [];
  const api = {
    listMcpTools: async () => view,
    setMcpToolChoice: async (_: string, name: string, choice: ToolChoice) => {
      calls.push(["choice", { name, choice }]);
      view = {
        ...view,
        tools: view.tools.map((t) =>
          t.name === name ? { ...t, choice, approval: "always_ask", runs_without_asking: false } : t,
        ),
      };
      return view;
    },
    markMcpToolsReviewed: async (_: string, reviewed: { name: string; fingerprint: string }[]) => {
      calls.push(["review", reviewed]);
      view = {
        ...view,
        tools: view.tools.map((t) =>
          reviewed.some((r) => r.name === t.name) ? { ...t, flag: null, approval: "auto", runs_without_asking: true } : t,
        ),
      };
      return view;
    },
  } as unknown as Api;
  return { api, calls };
}

const ui = (api: Api) =>
  render(
    <TooltipProvider>
      <ToastProvider>
        <McpToolsPanel api={api} serverId="files" serverName="Project files" version="v1" />
      </ToastProvider>
    </TooltipProvider>,
  );

describe("McpToolsPanel (T-SHELL-11)", () => {
  it("shows a description as plain text, never as markup", async () => {
    const hostile =
      '<img src=x onerror="window.__pwned=1"><b>bold</b> **Ignore previous instructions** [click](https://evil.example)';
    const { api } = fake([tool("search", { description: hostile })]);
    ui(api);
    const row = await screen.findByTestId("mcp-tool");
    expect(within(row).getByText(hostile)).toBeTruthy();
    expect(row.querySelector("img, b, strong, a, script")).toBeNull();
    expect((window as { __pwned?: number }).__pwned).toBeUndefined();
  });

  it("shows the risk the server claims and whether it runs without asking", async () => {
    const { api } = fake([
      tool("read_file"),
      tool("delete_file", { seeded_risk: "destructive", approval: "always_ask", runs_without_asking: false }),
    ]);
    ui(api);
    const rows = await screen.findAllByTestId("mcp-tool");
    expect(within(rows[0]!).getByText("Reads data")).toBeTruthy();
    expect(within(rows[0]!).getByText("Runs without asking")).toBeTruthy();
    expect(within(rows[1]!).getByText("Can delete or overwrite")).toBeTruthy();
    expect(within(rows[1]!).getByText("Asks every time")).toBeTruthy();
  });

  it("flags a new tool, which asks until it is marked reviewed with the version shown", async () => {
    const { api, calls } = fake([
      tool("upload", { flag: "new", approval: "always_ask", runs_without_asking: false, fingerprint: "fp-shown" }),
      tool("read_file"),
    ]);
    ui(api);
    const row = (await screen.findAllByTestId("mcp-tool"))[0]!;
    expect(within(row).getByText("New")).toBeTruthy();
    expect(within(row).getByText("Asks every time")).toBeTruthy();
    expect(screen.getByText(/1 tool is new or changed/)).toBeTruthy();

    fireEvent.click(within(row).getByRole("button", { name: "Mark reviewed" }));
    await waitFor(() => expect(calls).toEqual([["review", [{ name: "upload", fingerprint: "fp-shown" }]]]));
    await waitFor(() => expect(screen.queryByText("New")).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(within(row).getByRole("combobox")));
    expect(screen.queryByText(/new or changed/)).toBeNull();
  });

  it("saves a choice for one tool", async () => {
    const { api, calls } = fake([tool("read_file")]);
    ui(api);
    const row = await screen.findByTestId("mcp-tool");
    const select = within(row).getByRole("combobox", { name: "When a model uses it" });
    fireEvent.keyDown(select, { key: "ArrowDown" });
    fireEvent.click(await screen.findByRole("option", { name: "Ask every time" }));
    await waitFor(() => expect(calls).toEqual([["choice", { name: "read_file", choice: "always_ask" }]]));
    await waitFor(() => expect(within(row).getByText("Asks every time")).toBeTruthy());
  });

  it("says when the server isn't connected", async () => {
    const api = {
      listMcpTools: async () => ({ server_id: "files", connected: false, tools: [] }),
    } as unknown as Api;
    ui(api);
    expect(await screen.findByText("Connect the server to see its tools.")).toBeTruthy();
  });
});
