import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Api } from "../ipc/commands";
import { mockAddons } from "../ipc/mockAddons";
import type { AddonConsentRequest } from "../ipc/types";
import { AddonConsent } from "./AddonConsent";
import { AddonsPage } from "./Addons";
import { scheduleText, runText } from "./addonText";
import { TooltipProvider } from "./primitives";
import { ToastProvider } from "./toast";

afterEach(cleanup);

function api(over: Partial<Api> = {}): Api {
  return { ...mockAddons(), listProjects: async () => [], ...over } as unknown as Api;
}

const wrap = (ui: React.ReactElement) =>
  render(
    <TooltipProvider>
      <ToastProvider>{ui}</ToastProvider>
    </TooltipProvider>,
  );

describe("AddonConsent", () => {
  const request: AddonConsentRequest = {
    id: "brief",
    name: "Daily brief",
    version: "1.2.0",
    first: true,
    permissions: [
      { permission: "network", asked: false, allowed: false, tools: [], hosts: ["api.weather.example"] },
      { permission: "memory", asked: false, allowed: false, tools: [], hosts: [] },
    ],
  };

  it("preselects nothing, names where data goes, and sends only what was ticked", async () => {
    const addonConsent = vi.fn(async () => null);
    const onDone = vi.fn();
    wrap(<AddonConsent api={api({ addonConsent })} request={request} onDone={onDone} />);
    const dialog = screen.getByTestId("addon-consent");
    expect(within(dialog).getByText("What may Daily brief do?")).toBeTruthy();
    const boxes = within(dialog).getAllByRole("checkbox") as HTMLInputElement[];
    expect(boxes.every((b) => !b.checked)).toBe(true);
    expect(within(dialog).queryByText(/allow all/i)).toBeNull();
    // Shown in the consent order: memory before network.
    expect(boxes[0]?.labels?.[0]?.textContent).toBe("Remember things between chats");
    expect(within(dialog).getByText(/api\.weather\.example.*Delete all local data can't remove it/)).toBeTruthy();
    fireEvent.click(within(dialog).getByLabelText("Send data to the internet"));
    fireEvent.click(within(dialog).getByRole("button", { name: "Allow selected" }));
    await waitFor(() => expect(onDone).toHaveBeenCalledWith(true));
    expect(addonConsent).toHaveBeenCalledWith("brief", ["memory", "network"], {
      allowed: ["network"],
      chats_all: false,
      chats_projects: [],
      knowledge_projects: [],
    });
  });

  it("keeps the choices and says so when saving fails", async () => {
    const addonConsent = vi.fn(async () => {
      throw { code: "storage", message: "Disk full." };
    });
    const onDone = vi.fn();
    wrap(<AddonConsent api={api({ addonConsent })} request={request} onDone={onDone} />);
    fireEvent.click(screen.getByLabelText("Remember things between chats"));
    fireEvent.click(screen.getByRole("button", { name: "Allow selected" }));
    expect(await screen.findByText(/Couldn't save your choices/)).toBeTruthy();
    expect((screen.getByLabelText("Remember things between chats") as HTMLInputElement).checked).toBe(true);
    expect(onDone).not.toHaveBeenCalled();
  });

  it("Not now allows nothing", () => {
    const addonConsent = vi.fn(async () => null);
    const onDone = vi.fn();
    wrap(<AddonConsent api={api({ addonConsent })} request={request} onDone={onDone} />);
    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    expect(onDone).toHaveBeenCalledWith(false);
    expect(addonConsent).not.toHaveBeenCalled();
  });
});

describe("Settings › Add-ons", () => {
  it("lists add-ons with their state and opens one", async () => {
    wrap(<AddonsPage api={api()} />);
    const row = await screen.findByTestId("addon-row-brief");
    expect(within(row).getByText("Waiting for you · 1")).toBeTruthy();
    fireEvent.click(within(row).getByRole("button", { name: "Open Daily brief" }));
    const page = await screen.findByTestId("addon-page-brief");
    expect(within(page).getByRole("heading", { name: "Daily brief" })).toBeTruthy();
    expect(within(page).getByText("Read your chats")).toBeTruthy();
    expect(within(page).getByText(/hasn't been allowed anything yet/)).toBeTruthy();
    const job = within(page).getByTestId("addon-job-morning");
    expect(within(job).getByText("Every day at 07:00")).toBeTruthy();
    expect(within(job).getByText(/Missed at .* · Ran late at /)).toBeTruthy();
  });

  it("a held call shows the add-on, the job and the exact arguments, and Allow once sends their hash", async () => {
    const addonAnswerHeld = vi.fn(async () => null);
    wrap(<AddonsPage api={api({ addonAnswerHeld })} />);
    fireEvent.click(await screen.findByRole("button", { name: "Open Daily brief" }));
    const job = await screen.findByTestId("addon-job-morning");
    expect(within(job).getByText("Daily brief · Morning summary wants to")).toBeTruthy();
    expect(within(job).getByText(/"title": "Review the summary"/)).toBeTruthy();
    fireEvent.click(within(job).getByRole("button", { name: "Allow once" }));
    await waitFor(() =>
      expect(addonAnswerHeld).toHaveBeenCalledWith("00000000-0000-7000-8000-000000000002", true, "mock"),
    );
  });

  it("a list that can't be read says so with Retry, and shows no stale list", async () => {
    const addonsList = vi.fn(async () => {
      throw { code: "storage", message: "Database locked." };
    });
    wrap(<AddonsPage api={api({ addonsList })} />);
    expect(await screen.findByText(/Couldn't load your add-ons/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
    expect(screen.queryByTestId("addon-row-brief")).toBeNull();
  });
});

describe("words", () => {
  it("say schedules and runs plainly", () => {
    expect(scheduleText({ kind: "every", secs: 7200 })).toBe("Every 2 hours");
    expect(scheduleText({ kind: "daily_at", hour: 18, minute: 5 })).toBe("Every day at 18:05");
    const base = {
      id: "x",
      job: { plugin: "a", job: "b" },
      scheduled_for: 0,
      started_at: 0,
      finished_at: 0,
      late: false,
      covers_runs: 1,
      not_fresh: true,
      error: null,
    };
    expect(runText({ ...base, outcome: "skipped" })).toMatch(/no longer useful/);
    expect(runText({ ...base, outcome: "failed", error: "no answer" })).toMatch(/failed at .*: no answer/);
  });
});
