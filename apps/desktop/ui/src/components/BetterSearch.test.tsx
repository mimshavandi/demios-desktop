import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Api } from "../ipc/commands";
import type { KnowledgeStatus } from "../ipc/types";
import { BetterSearchOffer, offerText, progressText, upgradeText } from "./BetterSearch";

afterEach(cleanup);

const model = (state: KnowledgeStatus["model"] extends infer M ? (M extends { state: infer S } ? S : never) : never) => ({
  id: "nomic-embed-text-v2-moe-q5_k_m",
  name: "Nomic Embed Text v2 MoE",
  size: 370_828_256,
  license: "apache-2.0",
  state,
});

function fakeApi(initial: KnowledgeStatus) {
  let current = initial;
  const offer = vi.fn(async (accept: boolean) => {
    current = accept
      ? { offer: "accepted", model: model({ state: "downloading", done: 10, total: 100, bytes_per_sec: 0 }), embedding: null }
      : { ...current, offer: "declined" };
    return null;
  });
  const api = { knowledgeStatus: async () => current, knowledgeOffer: offer } as unknown as Api;
  return { api, offer };
}

const ask: KnowledgeStatus = { offer: "ask", model: model({ state: "not_downloaded" }), embedding: null };

describe("the embedding-model offer", () => {
  it("asks once, with the model's size and licence, when a project has files and no model", async () => {
    const { api } = fakeApi(ask);
    render(<BetterSearchOffer api={api} hasFiles />);
    await waitFor(() => expect(screen.getByTestId("better-search-offer").textContent).toContain(offerText(ask.model!)));
    expect(offerText(ask.model!)).toBe("Turn on better search? Downloads the 371 MB search model once. Licence: apache-2.0.");
  });

  it("asks someone who turned on the earlier model once more, saying the passages are re-indexed", async () => {
    const { api, offer } = fakeApi({ ...ask, offer: "upgrade" });
    render(<BetterSearchOffer api={api} hasFiles />);
    const prompt = await screen.findByTestId("better-search-offer");
    expect(prompt.textContent).toContain(upgradeText(ask.model!));
    expect(upgradeText(ask.model!)).toContain("371 MB");
    expect(upgradeText(ask.model!)).toContain("re-indexed");
    expect(upgradeText(ask.model!)).toContain("no files are read again");
    expect(upgradeText(ask.model!)).toContain("keywords only");
    expect(upgradeText(ask.model!)).toContain("Licence: apache-2.0.");
    fireEvent.click(screen.getByTestId("better-search-download"));
    await waitFor(() => expect(screen.getByTestId("better-search-progress").textContent).toContain("Downloading the search model"));
    expect(offer).toHaveBeenCalledWith(true);
  });

  it("doesn't ask for a project without files", async () => {
    const { api } = fakeApi(ask);
    const { container } = render(<BetterSearchOffer api={api} hasFiles={false} />);
    await new Promise((r) => setTimeout(r, 20));
    expect(container.textContent).toBe("");
  });

  it("'Not now' is saved and the prompt goes; a declined offer is never shown again", async () => {
    const { api, offer } = fakeApi(ask);
    render(<BetterSearchOffer api={api} hasFiles />);
    fireEvent.click(await screen.findByTestId("better-search-not-now"));
    await waitFor(() => expect(screen.queryByTestId("better-search-offer")).toBeNull());
    expect(offer).toHaveBeenCalledWith(false);
    cleanup();
    const declined = fakeApi({ ...ask, offer: "declined" });
    const { container } = render(<BetterSearchOffer api={declined.api} hasFiles />);
    await new Promise((r) => setTimeout(r, 20));
    expect(container.textContent).toBe("");
  });

  it("doesn't ask when the model is already installed", async () => {
    const { api } = fakeApi({ offer: "ask", model: model({ state: "ready" }), embedding: null });
    const { container } = render(<BetterSearchOffer api={api} hasFiles />);
    await new Promise((r) => setTimeout(r, 20));
    expect(container.textContent).toBe("");
  });

  it("'Download' starts it and shows the progress", async () => {
    const { api, offer } = fakeApi(ask);
    render(<BetterSearchOffer api={api} hasFiles />);
    fireEvent.click(await screen.findByTestId("better-search-download"));
    await waitFor(() => expect(screen.getByTestId("better-search-progress").textContent).toContain("Downloading the search model"));
    expect(offer).toHaveBeenCalledWith(true);
  });
});

describe("progress text", () => {
  it("covers downloading and improving search", () => {
    expect(progressText(null)).toBeNull();
    expect(progressText({ ...ask, model: model({ state: "ready" }), embedding: { done: 120, total: 480 } })).toBe(
      "Improving search: 120 of 480 passages.",
    );
    expect(progressText({ ...ask, model: model({ state: "verifying", total: 1 }) })).toBe("Downloading the search model…");
    expect(progressText({ ...ask, model: model({ state: "ready" }) })).toBeNull();
  });
});
