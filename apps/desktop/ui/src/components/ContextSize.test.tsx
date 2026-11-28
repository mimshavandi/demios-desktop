import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Api } from "../ipc/commands";
import type { ModelContext } from "../ipc/types";
import { ContextSize, ContextSizeTarget, parseSize } from "./ContextSize";

afterEach(cleanup);

function fake(initial: ModelContext) {
  let ctx = initial;
  const saved: [string, number | null][] = [];
  const api = {
    modelContext: async () => ctx,
    setModelContextSize: async (ref: string, tokens: number | null) => {
      saved.push([ref, tokens]);
      ctx = { tokens, source: tokens === null ? null : "user" };
      return ctx;
    },
  } as unknown as Api;
  return { api, saved };
}

const REF = "byok:openai:gpt-new";

describe("ContextSize (design ContextSize)", () => {
  it("says a known size and where it came from, with no field", async () => {
    const { api } = fake({ tokens: 200000, source: "published" });
    render(<ContextSize api={api} modelRef={REF} name="gpt-new" provider="OpenAI" />);
    expect(
      await screen.findByText("Reads up to 200,000 tokens at once (from Almend' list of published sizes)."),
    ).toBeTruthy();
    expect(screen.queryByLabelText("Context size")).toBeNull();
    cleanup();
    const p = fake({ tokens: 32768, source: "provider" });
    render(<ContextSize api={p.api} modelRef={REF} name="gpt-new" provider="LM Studio" />);
    expect(await screen.findByText("Reads up to 32,768 tokens at once (from LM Studio).")).toBeTruthy();
  });

  it("asks for an unknown size, refuses a bad one and saves a good one", async () => {
    const { api, saved } = fake({ tokens: null, source: null });
    const onSaved = vi.fn();
    render(
      <ContextSizeTarget.Provider value={{ focus: REF, onSaved }}>
        <ContextSize api={api} modelRef={REF} name="gpt-new" provider="OpenAI" />
      </ContextSizeTarget.Provider>,
    );
    expect(await screen.findByText("Context size needed")).toBeTruthy();
    const field = screen.getByLabelText("Context size");
    // Opened from a chat's "Set context size": the field has focus.
    await waitFor(() => expect(document.activeElement).toBe(field));
    fireEvent.change(field, { target: { value: "200k" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("Enter a whole number of tokens, from 1,024 to 10,000,000.")).toBeTruthy();
    expect(saved).toEqual([]);
    fireEvent.change(field, { target: { value: "128,000" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("Reads up to 128,000 tokens at once (you set this).")).toBeTruthy();
    expect(saved).toEqual([[REF, 128000]]);
    expect(onSaved).toHaveBeenCalledWith(REF);
    // Change shows the field again.
    fireEvent.click(screen.getByRole("button", { name: "Change context size for gpt-new" }));
    expect((screen.getByLabelText("Context size") as HTMLInputElement).value).toBe("128000");
  });

  it("parses whole numbers in range only", () => {
    expect(parseSize("1 024")).toBe(1024);
    expect(parseSize("10,000,000")).toBe(10_000_000);
    for (const bad of ["1023", "10000001", "12.5", "", "abc", "-5"]) expect(parseSize(bad)).toBeNull();
  });
});

describe("the chat's context size warning", () => {
  it("offers Set context size, then Retry once it's saved", async () => {
    const { ContextSizeWarning } = await import("./ChatView");
    const onSet = vi.fn();
    const onRetry = vi.fn();
    const msg = "Almend doesn't know how much text gpt-new can read at once, so it can't fit this chat to it.";
    const { rerender } = render(
      <ContextSizeWarning modelRef={REF} name="gpt-new" message={msg} saved={false} retry onRetry={onRetry} onSet={onSet} />,
    );
    expect(screen.getByText(msg)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Set context size for gpt-new in Settings" }));
    expect(onSet).toHaveBeenCalledWith(REF);
    rerender(
      <ContextSizeWarning modelRef={REF} name="gpt-new" message={msg} saved retry onRetry={onRetry} onSet={onSet} />,
    );
    expect(screen.getByText("Context size saved for gpt-new.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetry).toHaveBeenCalled();
  });
});
