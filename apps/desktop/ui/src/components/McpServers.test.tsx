import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { McpServerInput, McpServerStatus, McpServersView } from "../ipc/types";
import { McpServersCard } from "./McpServers";
import { TooltipProvider } from "./primitives";
import { ToastProvider } from "./toast";

afterEach(cleanup);

// jsdom has no ResizeObserver; Radix Switch measures itself with one.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

const tracker: McpServersView["servers"][number] = {
  id: "tracker",
  name: "Issue tracker",
  status: { server_id: "tracker", name: "Issue tracker", state: "needs_auth", message: "Sign in to use this server's tools.", tools: 0, review: 0 },
  transport: {
    type: "streamable_http",
    url: "https://mcp.tracker.example/mcp",
    headers: [{ name: "X-Workspace-Token", secret: true, value: null, stored: true }],
    oauth: { scopes: ["read"], client_id: null, signed_in: false },
  },
};

function fake(initial: McpServersView) {
  let view = initial;
  const calls: [string, unknown][] = [];
  const api = {
    listMcpServers: async () => view,
    saveMcpServer: async (input: McpServerInput, editing: string | null) => {
      calls.push(["save", { input, editing }]);
      if (input.id === "taken") throw { code: "invalid_input", message: "A server with the id taken already exists." };
      view = {
        ...view,
        servers: [
          ...view.servers.filter((s) => s.id !== input.id),
          { id: input.id, name: input.name ?? input.id, status: null, transport: { type: "stdio", command: "x", args: [], env: [] } },
        ],
      };
      return view;
    },
    removeMcpServer: async (id: string) => {
      calls.push(["remove", id]);
      view = { ...view, servers: view.servers.filter((s) => s.id !== id) };
      return view;
    },
    reconnectMcpServer: async (id: string) => (calls.push(["reconnect", id]), null),
    signInMcpServer: async (id: string) => (calls.push(["signin", id]), null),
    signOutMcpServer: async (id: string) => (calls.push(["signout", id]), view),
  } as unknown as Api;
  return { api, calls };
}

/** Opens a server's "More actions" menu and picks an item (#174). */
async function menuItem(server: string, item: string) {
  const trigger = await screen.findByRole("button", { name: `More actions for ${server}` });
  trigger.focus();
  fireEvent.keyDown(trigger, { key: "Enter" });
  fireEvent.click(await screen.findByRole("menuitem", { name: item }));
}

const ui = (api: Api, live: McpServerStatus[] = []) =>
  render(
    <ToastProvider>
      <TooltipProvider>
        <McpServersCard api={api} live={live} />
      </TooltipProvider>
    </ToastProvider>,
  );

describe("McpServersCard", () => {
  it("adds a local server: secrets are sent once and the form keeps nothing", async () => {
    const { api, calls } = fake({ servers: [], local_servers_allowed: true });
    ui(api);
    expect(await screen.findByText("No tool servers yet")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add tool server" }));
    expect(document.activeElement?.textContent).toBe("Add a tool server");

    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Work Files" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "/usr/bin/mcp-files" } });
    fireEvent.change(screen.getByLabelText("Arguments"), { target: { value: "--root\n /tmp \n\n" } });
    fireEvent.click(screen.getByRole("button", { name: "Add variable" }));
    fireEvent.change(screen.getByLabelText("Name of variable"), { target: { value: "API_TOKEN" } });
    // A secret-looking name turns into a secret field.
    const value = screen.getByLabelText("Secret value") as HTMLInputElement;
    expect(value.type).toBe("password");
    fireEvent.change(value, { target: { value: "s3cret" } });
    fireEvent.click(screen.getByRole("button", { name: "Add tool server" }));

    await waitFor(() => expect(calls[0]?.[0]).toBe("save"));
    expect(calls[0]?.[1]).toEqual({
      editing: null,
      input: {
        id: "work-files",
        name: "Work Files",
        transport: {
          type: "stdio",
          command: "/usr/bin/mcp-files",
          args: ["--root", "/tmp"],
          env: [{ name: "API_TOKEN", secret: true, value: "s3cret" }],
        },
      },
    });
    expect(await screen.findByText("Work Files")).toBeTruthy();
  });

  it("edits a server without resending its stored secret", async () => {
    const { api, calls } = fake({ servers: [tracker], local_servers_allowed: true });
    ui(api);
    await menuItem("Issue tracker", "Edit");
    const secret = screen.getByLabelText("Secret value") as HTMLInputElement;
    expect(secret.value).toBe("");
    expect(secret.placeholder).toBe("Saved");
    expect((screen.getByLabelText("ID") as HTMLInputElement).readOnly).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(calls.length).toBe(1));
    const saved = calls[0]?.[1] as { input: McpServerInput; editing: string };
    expect(saved.editing).toBe("tracker");
    expect(saved.input.transport).toEqual({
      type: "streamable_http",
      url: "https://mcp.tracker.example/mcp",
      headers: [{ name: "X-Workspace-Token", secret: true, value: null }],
      oauth: true,
      scopes: ["read"],
      client_id: null,
    });
  });

  it("shows save errors in the form", async () => {
    const { api } = fake({ servers: [], local_servers_allowed: true });
    ui(api);
    fireEvent.click(await screen.findByRole("button", { name: "Add tool server" }));
    fireEvent.change(screen.getByLabelText("ID"), { target: { value: "taken" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "x" } });
    fireEvent.click(screen.getByRole("button", { name: "Add tool server" }));
    expect(await screen.findByText(/already exists/)).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Add a tool server" })).toBeTruthy();
  });

  it("signs in and out, follows live status, and removes only after a confirm", async () => {
    const { api, calls } = fake({ servers: [tracker], local_servers_allowed: true });
    const { rerender } = ui(api);
    const row = await screen.findByTestId("mcp-server-row");
    expect(within(row).getByText("Needs sign-in")).toBeTruthy();
    fireEvent.click(within(row).getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(calls).toContainEqual(["signin", "tracker"]));

    const connected: McpServerStatus = { server_id: "tracker", name: "Issue tracker", state: "connected", message: null, tools: 2, review: 0 };
    const signedIn = { ...tracker, transport: { ...tracker.transport, oauth: { scopes: ["read"], client_id: null, signed_in: true } } } as typeof tracker;
    const after = fake({ servers: [signedIn], local_servers_allowed: true });
    rerender(
      <ToastProvider>
        <TooltipProvider>
          <McpServersCard api={after.api} live={[connected]} />
        </TooltipProvider>
      </ToastProvider>,
    );
    expect(await screen.findByText("2 tools")).toBeTruthy();
    expect(screen.getByText("Connected")).toBeTruthy();

    await menuItem("Issue tracker", "Remove");
    const dialog = await screen.findByRole("alertdialog", { name: "Remove Issue tracker?" });
    expect(dialog.textContent).toContain("1 secret saved for it in your system keychain");
    expect(dialog.textContent).toContain("can't be undone");
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(after.calls.some((c) => c[0] === "remove")).toBe(false);

    await menuItem("Issue tracker", "Remove");
    const confirm = (await screen.findByRole("button", { name: "Remove server" })) as HTMLButtonElement;
    await waitFor(() => expect(confirm.disabled).toBe(false), { timeout: 1500 });
    fireEvent.click(confirm);
    await waitFor(() => expect(after.calls).toContainEqual(["remove", "tracker"]));
    expect(await screen.findByText("No tool servers yet")).toBeTruthy();
  });

  it("returns focus to the trigger, asks before discarding changes, and asks to retype secrets for a new address", async () => {
    const { api } = fake({ servers: [tracker], local_servers_allowed: true });
    ui(api);
    await menuItem("Issue tracker", "Edit");
    // Unchanged: Cancel closes at once and focus goes back to Edit.
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("More actions for Issue tracker"));

    await menuItem("Issue tracker", "Edit");
    fireEvent.change(screen.getByLabelText("Server address"), { target: { value: "https://new.example/mcp" } });
    expect((screen.getByLabelText("Secret value") as HTMLInputElement).placeholder).toBe("Type it again");
    expect(screen.getByText(/saved secrets aren't reused/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByText("You have unsaved changes.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
    expect((screen.getByLabelText("Server address") as HTMLInputElement).value).toBe("https://new.example/mcp");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
    expect(await screen.findByRole("button", { name: "More actions for Issue tracker" })).toBeTruthy();
  });

  it("new arguments for a local server ask for its secrets again", async () => {
    const local: McpServersView["servers"][number] = {
      id: "pkg",
      name: "Package",
      status: null,
      transport: { type: "stdio", command: "npx", args: ["pkg-a"], env: [{ name: "API_TOKEN", secret: true, value: null, stored: true }] },
    };
    const { api } = fake({ servers: [local], local_servers_allowed: true });
    ui(api);
    await menuItem("Package", "Edit");
    const secret = screen.getByLabelText("Secret value") as HTMLInputElement;
    expect(secret.placeholder).toBe("Saved");
    fireEvent.change(screen.getByLabelText("Arguments"), { target: { value: "pkg-b" } });
    expect(secret.placeholder).toBe("Type it again");
  });

  it("the remove confirm lists the chat approvals that go too", async () => {
    const { api } = fake({ servers: [tracker], local_servers_allowed: true });
    ui(api);
    await menuItem("Issue tracker", "Remove");
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog.textContent).toContain("This stops the server");
    expect(dialog.textContent).toContain('"Allow in this chat" approvals for its tools');
  });

  it("offers only remote servers where local programs can't run", async () => {
    const { api } = fake({ servers: [], local_servers_allowed: false });
    ui(api);
    fireEvent.click(await screen.findByRole("button", { name: "Add tool server" }));
    expect(screen.queryByLabelText(/On this computer/)).toBeNull();
    expect((screen.getByLabelText(/Remote/) as HTMLInputElement).checked).toBe(true);
    expect(screen.getByLabelText("Server address")).toBeTruthy();
    expect(screen.getByText(/can't start programs on your computer/)).toBeTruthy();
  });

  it("shows how many tools need review and opens a server's tools", async () => {
    const { api } = fake({ servers: [tracker], local_servers_allowed: true });
    let listed = 0;
    const withTools = {
      ...api,
      listMcpTools: async (serverId: string) => (
        listed++,
        { server_id: serverId, connected: true, tools: [] }
      ),
    } as Api;
    const live: McpServerStatus = { server_id: "tracker", name: "Issue tracker", state: "connected", message: null, tools: 2, review: 2 };
    ui(withTools, [live]);
    const row = await screen.findByTestId("mcp-server-row");
    expect(within(row).getByText("2 to review")).toBeTruthy();
    const toggle = within(row).getByRole("button", { name: "Tools of Issue tracker" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(await screen.findByRole("region", { name: "Tools of Issue tracker" })).toBeTruthy();
    await waitFor(() => expect(listed).toBe(1));
    fireEvent.click(toggle);
    expect(screen.queryByRole("region", { name: "Tools of Issue tracker" })).toBeNull();
  });
});
