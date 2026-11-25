import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { MessageSources } from "../ipc/types";
import { KEYWORD_ONLY, Sources, configureSources } from "./Sources";

afterEach(cleanup);

const cite = { label: "S1", chunk_id: 1, file_id: "f", display_path: "docs/plan.md", start_line: 3, end_line: 9 };

describe("Sources", () => {
  it("shows loading, then the files and lines the answer was given", async () => {
    let resolve: (s: MessageSources | null) => void = () => {};
    configureSources(() => new Promise((r) => (resolve = r)));
    render(<Sources messageId="m1" />);
    expect(screen.getByTestId("sources-loading")).toBeTruthy();
    resolve({ mode: "hybrid", citations: [cite] });
    await waitFor(() => expect(screen.getByTestId("source").textContent).toBe("[S1] docs/plan.md lines 3–9"));
    expect(screen.queryByTestId("sources-keyword-only")).toBeNull();
  });

  it("labels keyword-only search and says when nothing matched", async () => {
    configureSources(async () => ({ mode: "keyword", citations: [] }));
    render(<Sources messageId="m2" />);
    await waitFor(() => expect(screen.getByTestId("sources-empty")).toBeTruthy());
    expect(screen.getByTestId("sources-keyword-only").textContent).toBe(KEYWORD_ONLY);
  });

  it("shows nothing outside a project chat", async () => {
    configureSources(async () => null);
    const { container } = render(<Sources messageId="m3" />);
    await waitFor(() => expect(container.textContent).toBe(""));
  });

  it("says when the sources can't be loaded, and can try again", async () => {
    let fail = true;
    configureSources(async () => {
      if (fail) throw new Error("db");
      return { mode: "hybrid", citations: [cite] };
    });
    render(<Sources messageId="m4" />);
    await waitFor(() => expect(screen.getByTestId("sources-error")).toBeTruthy());
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(screen.getByTestId("source")).toBeTruthy());
  });
});
