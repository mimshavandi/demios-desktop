import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Api } from "../ipc/commands";
import type { HubPullEvent, HubRepoView, LocalModelEvent, LocalModelRow } from "../ipc/types";
import { AddFromHub } from "./AddFromHub";
import { LocalModelsCard } from "./LocalModels";
import { TooltipProvider } from "./primitives";
import { ToastProvider } from "./toast";

afterEach(cleanup);

const fit = (status: "ready" | "memory_blocked", estimated: boolean) => ({
  status,
  estimated,
  reason: status === "ready" ? "Fits." : "Won't fit.",
  suggested_quant: null,
});

const repoView = (over: Partial<HubRepoView> = {}): HubRepoView => ({
  id: "org/Tiny-GGUF",
  sha: "c".repeat(40),
  gated: false,
  license: { id: "apache-2.0", name: null, link: null, warning: null },
  card_url: "https://huggingface.co/org/Tiny-GGUF",
  variants: [
    { name: "Tiny-Q4_K_M", quant: "Q4_K_M", total_size: 1e9, parts: 1, verifiable: true, fit: fit("ready", false), downloaded: null },
    { name: "Tiny-F16", quant: "F16", total_size: 4e9, parts: 2, verifiable: false, fit: fit("memory_blocked", true), downloaded: null },
  ],
  ...over,
});

function fake(view = repoView()) {
  const calls: unknown[][] = [];
  let pullEvents: ((e: HubPullEvent) => void) | null = null;
  let finishPull: ((ref: string) => void) | null = null;
  let failPull: ((e: unknown) => void) | null = null;
  let searchFails = true;
  const api = {
    hubSearch: async (text: string, sort: string, cursor: string | null) => {
      calls.push(["search", text, sort, cursor]);
      if (searchFails) {
        searchFails = false;
        throw { code: "capability", message: "Couldn't reach Hugging Face. Check your connection." };
      }
      if (text === "none") return { items: [], next: null };
      const item = (id: string) => ({ id, author: "org", downloads: 1200, likes: 3, last_modified: null, gated: false, pipeline_tag: null, license: "apache-2.0", tags: [] });
      return cursor === null ? { items: [item("org/Tiny-GGUF")], next: "page2" } : { items: [item("org/Other-GGUF")], next: null };
    },
    hubRepo: async (repo: string) => {
      calls.push(["repo", repo]);
      return view;
    },
    hubPull: (repo: string, sha: string, variant: string, onEvent: (e: HubPullEvent) => void) => {
      calls.push(["pull", repo, sha, variant]);
      pullEvents = onEvent;
      return new Promise<string>((res, rej) => {
        finishPull = res;
        failPull = rej;
      });
    },
    hubCancel: async (...a: string[]) => (calls.push(["cancel", ...a]), true),
    hfTokenStatus: async () => false,
    setHfToken: async (t: string) => (calls.push(["token", t]), true),
    deleteHfToken: async () => false,
  } as unknown as Api;
  return {
    api,
    calls,
    progress: (done: number, total: number) => pullEvents?.({ type: "progress", part: 0, parts: 1, done_bytes: done, total_bytes: total }),
    finish: (ref: string) => finishPull?.(ref),
    fail: (e: unknown) => failPull?.(e),
  };
}

const ui = (el: React.ReactElement) =>
  render(
    <ToastProvider>
      <TooltipProvider>{el}</TooltipProvider>
    </ToastProvider>,
  );

describe("AddFromHub", () => {
  it("searches with retry, empty results and paging", async () => {
    const f = fake();
    ui(<AddFromHub api={f.api} onClose={() => {}} />);
    fireEvent.change(screen.getByLabelText("Search models"), { target: { value: "tiny" } });
    fireEvent.click(screen.getByRole("button", { name: "Search" }));
    expect(await screen.findByText(/Couldn't reach Hugging Face/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("button", { name: /org\/Tiny-GGUF/ })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show more" }));
    expect(await screen.findByRole("button", { name: /org\/Other-GGUF/ })).toBeTruthy();
    expect(f.calls).toContainEqual(["search", "tiny", "downloads", "page2"]);

    fireEvent.change(screen.getByLabelText("Search models"), { target: { value: "none" } });
    fireEvent.click(screen.getByRole("button", { name: "Search" }));
    expect(await screen.findByText("No GGUF models match “none”.")).toBeTruthy();
  });

  it("shows fit, licence and card first; downloads with progress; unverifiable can't download", async () => {
    const f = fake();
    ui(<AddFromHub api={f.api} onClose={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Search" }));
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    fireEvent.click(await screen.findByRole("button", { name: /org\/Tiny-GGUF/ }));
    expect(screen.getByText(/Checking model…/)).toBeTruthy();

    const [q4, f16] = await screen.findAllByTestId("hub-variant");
    expect(q4!.textContent).toContain("Ready");
    expect(q4!.textContent).not.toContain("(estimate)");
    expect(f16!.textContent).toContain("Won't fit (estimate)");
    expect(f16!.textContent).toContain("No published checksum");
    expect((within(f16!).getByRole("button", { name: "Download" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(/Licence: apache-2.0/)).toBeTruthy();
    expect(screen.getByText("huggingface.co/org/Tiny-GGUF")).toBeTruthy();

    fireEvent.click(within(q4!).getByRole("button", { name: "Download" }));
    act(() => f.progress(250e6, 1e9));
    const bar = await within(q4!).findByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("250000000");
    fireEvent.click(within(q4!).getByRole("button", { name: "Pause" }));
    expect(f.calls).toContainEqual(["cancel", "org/Tiny-GGUF", "c".repeat(40), "Tiny-Q4_K_M"]);
    act(() => f.finish("local:hf/org/Tiny-GGUF/Tiny-Q4_K_M"));
    expect(await within(q4!).findByText("Downloaded")).toBeTruthy();
  });

  it("a failed download says why and offers Retry", async () => {
    const f = fake();
    ui(<AddFromHub api={f.api} onClose={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Search" }));
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    fireEvent.click(await screen.findByRole("button", { name: /org\/Tiny-GGUF/ }));
    const [q4] = await screen.findAllByTestId("hub-variant");
    fireEvent.click(within(q4!).getByRole("button", { name: "Download" }));
    act(() => f.fail({ code: "capability", message: "Not enough disk space." }));
    expect(await within(q4!).findByText("Not enough disk space.")).toBeTruthy();
    expect(within(q4!).getByRole("button", { name: "Retry" })).toBeTruthy();
  });

  it("gated models ask for a token and warn about the licence", async () => {
    const f = fake(repoView({ gated: true, license: { id: "cc-by-nc-4.0", name: null, link: null, warning: "non_commercial" } }));
    ui(<AddFromHub api={f.api} onClose={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Search" }));
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    fireEvent.click(await screen.findByRole("button", { name: /org\/Tiny-GGUF/ }));
    expect(await screen.findByText(/doesn't allow commercial use/)).toBeTruthy();
    const token = (await screen.findByLabelText("Hugging Face token")) as HTMLInputElement;
    expect(token.type).toBe("password");
    fireEvent.change(token, { target: { value: "hf_abc" } });
    fireEvent.click(screen.getByRole("button", { name: "Save token" }));
    expect(await screen.findByText("Hugging Face token saved.")).toBeTruthy();
    expect(f.calls).toContainEqual(["token", "hf_abc"]);
  });
});

describe("LocalModelsCard with Hub and cache models", () => {
  it("adds rows from events, labels where they come from, and removes them", async () => {
    let sink: ((e: LocalModelEvent) => void) | null = null;
    const api = {
      watchLocalModels: async (on: (e: LocalModelEvent) => void) => ((sink = on), []),
    } as unknown as Api;
    ui(<LocalModelsCard api={api} initial={[]} onReady={() => {}} />);
    await waitFor(() => expect(sink).not.toBeNull());
    const row: LocalModelRow = {
      id: "hfcache/org/M/M-Q8_0",
      origin: "hf_cache",
      location: "/Users/me/.cache/huggingface/hub/models--org--M/snapshots/x/M-Q8_0.gguf",
      model_ref: "local:hfcache/org/M/M-Q8_0",
      name: "M",
      repo: "org/M",
      quant: "Q8_0",
      size: 8e9,
      params_b: 0,
      capabilities: ["chat"],
      license: "",
      license_warning: true,
      card_url: "https://huggingface.co/org/M",
      notes: "",
      state: { state: "ready" },
      fit: fit("ready", false),
    };
    act(() => sink?.({ type: "changed", row }));
    expect(await screen.findByText(/From your Hugging Face cache, used where it is/)).toBeTruthy();
    expect(screen.getByText(row.location!)).toBeTruthy();
    act(() => sink?.({ type: "removed", id: row.id }));
    await waitFor(() => expect(screen.queryByText(/From your Hugging Face cache/)).toBeNull());
  });
});
