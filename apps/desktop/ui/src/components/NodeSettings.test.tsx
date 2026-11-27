import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { JoinStarted, NodeOverview } from "../ipc/types";
import { lastSyncedText, NodeSettings, thisDeviceSyncText, whenText } from "./NodeSettings";
import { TooltipProvider } from "./primitives";
import { ToastProvider } from "./toast";

afterEach(cleanup);

globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

const NOW = Date.now();
const none: NodeOverview = {
  paired: null,
  online: false,
  status: null,
  devices: [],
  problem: null,
  sync: { state: "off", last_synced_at: null, failures: 0, error: null, applied: 0 },
};
const paired: NodeOverview = {
  paired: { name: "Home server", address: "192.168.1.20:7443", paired_at: NOW, last_reached_at: NOW },
  online: true,
  status: {
    name: "Home server",
    version: "1.4.0",
    secret_store: "systemd_credential",
    secret_store_label: "Encrypted file, key from a systemd credential",
    protected_at_rest: true,
    devices: 3,
    reachable_from_outside: false,
  },
  devices: [
    { id: "n1", name: "Home server", kind: "node", last_synced_at: NOW, this_device: false },
    { id: "d1", name: "MacBook Pro", kind: "desktop", last_synced_at: NOW, this_device: true },
    { id: "p1", name: "Work phone", kind: "phone", last_synced_at: NOW - 5 * 60_000, this_device: false },
  ],
  problem: null,
  sync: { state: "up_to_date", last_synced_at: NOW, failures: 0, error: null, applied: 0 },
};

function fake(start: NodeOverview, over: Partial<Api> = {}) {
  let view = start;
  const calls: unknown[][] = [];
  let release: (v: NodeOverview) => void = () => {};
  const api = {
    nodeOverview: async () => view,
    nodeJoinBegin: async (link: string): Promise<JoinStarted> => {
      calls.push(["begin", link]);
      return { numbers: ["4821", "0937", "5512"], address: "192.168.1.20:7443" };
    },
    nodeJoinWait: () =>
      new Promise<NodeOverview>((resolve) => {
        release = (v) => {
          view = v;
          resolve(v);
        };
      }),
    nodeJoinCancel: async () => {
      calls.push(["cancel"]);
      return null;
    },
    nodeRemoveDevice: async (id: string) => {
      calls.push(["remove", id]);
      view = { ...view, devices: view.devices.filter((d) => d.id !== id) };
      return view;
    },
    nodeUnpair: async () => {
      calls.push(["unpair"]);
      view = none;
      return view;
    },
    nodeForget: async () => {
      calls.push(["forget"]);
      view = none;
      return view;
    },
    nodeSyncNow: async () => {
      calls.push(["sync"]);
      view = { ...view, sync: { ...view.sync, state: "syncing", error: null, failures: 0 } };
      return view;
    },
    ...over,
  } as unknown as Api;
  return { api, calls, confirmOnNode: (v: NodeOverview) => release(v) };
}

function show(api: Api) {
  return render(
    <TooltipProvider>
      <ToastProvider>
        <NodeSettings api={api} />
      </ToastProvider>
    </TooltipProvider>,
  );
}

describe("Settings › Node (ADR-0014)", () => {
  it("with no Node, says what a Node holds and offers to pair one", async () => {
    show(fake(none).api);
    expect(await screen.findByRole("heading", { name: "No Node paired" })).toBeTruthy();
    expect(screen.getByText(/unencrypted/)).toBeTruthy();
    expect(screen.getByText(/Only pair one you control/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pair a Node" })).toBeTruthy();
  });

  it("joins with a link: shows the check numbers, waits for the Node, then shows it paired", async () => {
    const f = fake(none);
    show(f.api);
    fireEvent.click(await screen.findByRole("button", { name: "Pair a Node" }));
    const dialog = await screen.findByTestId("join-node-dialog");
    const connect = within(dialog).getByRole("button", { name: "Connect" }) as HTMLButtonElement;
    expect(connect.disabled).toBe(true);
    fireEvent.change(within(dialog).getByLabelText("Pairing link"), {
      target: { value: "  demios://pair?v=1&a=nas:7443  " },
    });
    fireEvent.click(connect);
    const numbers = await within(dialog).findByTestId("check-numbers");
    expect(numbers.textContent).toBe("4821 0937 5512");
    expect(numbers.getAttribute("aria-label")).toBe("4821, 0937, 5512");
    expect(within(dialog).getByText("Waiting for you to confirm on the Node…")).toBeTruthy();
    expect(f.calls[0]).toEqual(["begin", "demios://pair?v=1&a=nas:7443"]);
    f.confirmOnNode(paired);
    expect(await screen.findByRole("heading", { name: /Home server/ })).toBeTruthy();
    expect(screen.queryByTestId("join-node-dialog")).toBeNull();
  });

  it("'They don't match' stops pairing and says nothing was shared", async () => {
    const f = fake(none);
    show(f.api);
    fireEvent.click(await screen.findByRole("button", { name: "Pair a Node" }));
    const dialog = await screen.findByTestId("join-node-dialog");
    fireEvent.change(within(dialog).getByLabelText("Pairing link"), { target: { value: "demios://pair?x" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
    fireEvent.click(await within(dialog).findByRole("button", { name: "They don't match" }));
    expect(await within(dialog).findByText(/Pairing stopped. Nothing was shared/)).toBeTruthy();
    expect(f.calls).toContainEqual(["cancel"]);
  });

  it("a bad link shows the reason in the dialog, which stays open", async () => {
    const f = fake(none, {
      nodeJoinBegin: async () => {
        throw { code: "node_link", message: "This link has expired or was already used. Run dn node pair again for a new one." };
      },
    } as Partial<Api>);
    show(f.api);
    fireEvent.click(await screen.findByRole("button", { name: "Pair a Node" }));
    const dialog = await screen.findByTestId("join-node-dialog");
    fireEvent.change(within(dialog).getByLabelText("Pairing link"), { target: { value: "demios://pair?old" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
    expect(await within(dialog).findByRole("alert")).toBeTruthy();
    expect(within(dialog).getByText(/expired or was already used/)).toBeTruthy();
    expect(within(dialog).getByLabelText("Pairing link")).toBeTruthy();
  });

  it("paired and online: the Node's facts and the devices, with this device marked", async () => {
    show(fake(paired).api);
    expect(await screen.findByText("Online")).toBeTruthy();
    expect(screen.getByText("1.4.0")).toBeTruthy();
    expect(screen.getByText("192.168.1.20:7443")).toBeTruthy();
    expect(screen.getByText("Encrypted file, key from a systemd credential")).toBeTruthy();
    expect(screen.queryByText("Unprotected")).toBeNull();
    expect(screen.getByText("Devices on its network")).toBeTruthy();
    expect(screen.getByText("This device")).toBeTruthy();
    expect(screen.getByText("Last synced 5 min ago")).toBeTruthy();
    // The Node itself has no menu; the others do.
    expect(screen.queryByRole("button", { name: "More for Home server" })).toBeNull();
    expect(screen.getByRole("button", { name: "More for Work phone" })).toBeTruthy();
  });

  it("a plaintext store is flagged Unprotected", async () => {
    const plain: NodeOverview = {
      ...paired,
      status: { ...paired.status!, secret_store: "insecure_plaintext", secret_store_label: "Plaintext file (unprotected, for testing only)", protected_at_rest: false },
    };
    show(fake(plain).api);
    expect(await screen.findByText("Unprotected")).toBeTruthy();
  });

  it("a Node reachable from outside says so, with what that means", async () => {
    show(fake({ ...paired, status: { ...paired.status!, reachable_from_outside: true } }).api);
    expect(await screen.findByText("Reachable from outside your network")).toBeTruthy();
    expect(screen.getByText(/Anyone who can reach its port can try to connect/)).toBeTruthy();
    expect(screen.queryByText("Devices on its network")).toBeNull();
  });

  it("an older Node that doesn't report who can connect shows no row", async () => {
    const { reachable_from_outside: _, ...older } = paired.status!;
    show(fake({ ...paired, status: older }).api);
    expect(await screen.findByText("Online")).toBeTruthy();
    expect(screen.queryByText("Who can connect")).toBeNull();
  });

  it("offline: says since when and that local changes are kept, with Retry", async () => {
    const at = new Date(NOW);
    at.setHours(3, 12);
    const offline: NodeOverview = {
      ...paired,
      online: false,
      status: null,
      devices: [],
      paired: { ...paired.paired!, last_reached_at: at.getTime() },
    };
    show(fake(offline).api);
    expect(await screen.findByText("Offline")).toBeTruthy();
    expect(screen.getByText(/Last reached .*Changes made here are kept/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
    expect(screen.getByText("The device list shows when your Node answers.")).toBeTruthy();
  });

  it("a problem (version mismatch) is shown as an error, not as offline", async () => {
    const mismatch: NodeOverview = {
      ...paired,
      online: false,
      status: null,
      devices: [],
      problem: "the other device speaks sync version 2 and this one 1. Update this app to sync",
    };
    show(fake(mismatch).api);
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByText(/Update this app to sync/)).toBeTruthy();
  });

  it("a Node that is gone or removed this computer can be forgotten here, after a confirmation", async () => {
    const gone: NodeOverview = { ...paired, online: false, status: null, devices: [] };
    const f = fake(gone);
    show(f.api);
    fireEvent.click(await screen.findByRole("button", { name: "Forget this Node" }));
    const confirm = await screen.findByRole("alertdialog");
    expect(within(confirm).getByText(/still lists this computer/)).toBeTruthy();
    expect(within(confirm).getByText("dn node remove-device")).toBeTruthy();
    const button = within(confirm).getByRole("button", { name: "Forget this Node" }) as HTMLButtonElement;
    await waitFor(() => expect(button.disabled).toBe(false));
    fireEvent.click(button);
    expect(await screen.findByRole("heading", { name: "No Node paired" })).toBeTruthy();
    expect(f.calls).toContainEqual(["forget"]);
  });

  it("an online Node doesn't offer Forget (Unpair removes this computer there too)", async () => {
    show(fake(paired).api);
    await screen.findByText("Online");
    expect(screen.queryByRole("button", { name: "Forget this Node" })).toBeNull();
  });

  it("removing a device asks first, then removes it", async () => {
    const f = fake(paired);
    show(f.api);
    fireEvent.pointerDown(await screen.findByRole("button", { name: "More for Work phone" }), { button: 0 });
    fireEvent.click(await screen.findByRole("menuitem", { name: "Remove device" }));
    const confirm = await screen.findByRole("alertdialog");
    expect(within(confirm).getByText("Remove Work phone?")).toBeTruthy();
    expect(within(confirm).getByText(/Its copy of your data stays on it/)).toBeTruthy();
    const button = within(confirm).getByRole("button", { name: "Remove device" }) as HTMLButtonElement;
    await waitFor(() => expect(button.disabled).toBe(false));
    fireEvent.click(button);
    await waitFor(() => expect(f.calls).toContainEqual(["remove", "p1"]));
    await waitFor(() => expect(screen.queryByText("Work phone")).toBeNull());
  });

  it("this device's menu offers to unpair", async () => {
    const f = fake(paired);
    show(f.api);
    fireEvent.pointerDown(await screen.findByRole("button", { name: "More for MacBook Pro" }), { button: 0 });
    fireEvent.click(await screen.findByRole("menuitem", { name: "Unpair this device" }));
    const confirm = await screen.findByRole("alertdialog");
    expect(within(confirm).getByText("This device stops syncing. Your data stays here.")).toBeTruthy();
    const button = within(confirm).getByRole("button", { name: "Unpair this device" }) as HTMLButtonElement;
    await waitFor(() => expect(button.disabled).toBe(false));
    fireEvent.click(button);
    expect(await screen.findByRole("heading", { name: "No Node paired" })).toBeTruthy();
    expect(f.calls).toContainEqual(["unpair"]);
  });
});

describe("time text", () => {
  it("is short and plain", () => {
    const now = new Date(2026, 10, 26, 12, 0).getTime();
    expect(lastSyncedText(null, now)).toBe("Not synced yet");
    expect(lastSyncedText(now - 10_000, now)).toBe("Last synced just now");
    expect(lastSyncedText(now - 2 * 60_000, now)).toBe("Last synced 2 min ago");
    expect(whenText(new Date(2026, 10, 26, 3, 12).getTime(), now)).toMatch(/03:12|3:12/);
    expect(whenText(new Date(2026, 10, 25, 3, 12).getTime(), now)).toMatch(/25/);
  });
});

describe("sync", () => {
  it("says where sync stands on this device's row", () => {
    const base = paired.sync;
    expect(thisDeviceSyncText({ ...base, state: "syncing" }, NOW)).toBe("Syncing now");
    expect(thisDeviceSyncText({ ...base, last_synced_at: NOW - 2 * 60_000 }, NOW)).toBe(
      "Up to date · synced 2 min ago",
    );
    expect(thisDeviceSyncText({ ...base, state: "retrying" }, NOW)).toBe("Trying again soon");
  });

  it("shows a failure that will be tried again, without a button", async () => {
    const { api } = fake({
      ...paired,
      sync: { ...paired.sync, state: "retrying", failures: 1, error: "Couldn't reach your Node." },
    });
    show(api);
    expect(await screen.findByText("Couldn't reach your Node. Trying again soon.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Sync now" })).toBeNull();
  });

  it("says why sync stopped and starts it again on Sync now", async () => {
    const { api, calls } = fake({
      ...paired,
      sync: {
        ...paired.sync,
        state: "stopped",
        failures: 5,
        error: "Your Node removed this computer. Forget the Node here, then pair again.",
      },
    });
    show(api);
    expect(
      await screen.findByText(
        "Sync stopped: your Node removed this computer. Forget the Node here, then pair again.",
      ),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Sync now" }));
    await waitFor(() => expect(calls).toContainEqual(["sync"]));
    expect(await screen.findByText("Syncing now")).toBeTruthy();
    expect(screen.queryByText(/Sync stopped/)).toBeNull();
  });
});
