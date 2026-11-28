import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { COPY_HELP, FileChips, fileSize, middleCut, providerHelp } from "./ChatAttachments";
import { TooltipProvider } from "./primitives";

afterEach(cleanup);

const file = {
  id: "f1",
  conversation_id: "c1",
  name: "umsatz-filialen-2026-quartal-3.csv",
  media_type: "text/csv",
  size: 2_400_000,
  sha256: "0".repeat(64),
  created_at: 0,
};

describe("ChatAttachments", () => {
  it("sizes and long names read as designed", () => {
    expect(fileSize(676)).toBe("676 B");
    expect(fileSize(2_458)).toBe("2.5 KB");
    expect(fileSize(2_400_000)).toBe("2.4 MB");
    expect(middleCut("orders.csv")).toBe("orders.csv");
    const cut = middleCut(file.name);
    expect(cut).toHaveLength(28);
    expect(cut.endsWith("quartal-3.csv")).toBe(true);
    expect(cut).toContain("…");
  });

  it("names the provider only when the model isn't on this device", () => {
    const chips = (provider: string | null) =>
      render(
        <TooltipProvider>
          <FileChips files={[file]} provider={provider} onRemove={() => {}} removeRef={() => {}} />
        </TooltipProvider>,
      );
    chips(null);
    expect(screen.getByText(COPY_HELP)).toBeTruthy();
    expect(screen.queryByText(/gets what the data tools compute/)).toBeNull();
    expect(screen.getByTitle(file.name)).toBeTruthy();
    expect(screen.getByRole("list", { name: "Files in this chat" })).toBeTruthy();
    cleanup();
    chips("Anthropic");
    expect(screen.getByText(new RegExp(providerHelp("Anthropic").slice(0, 40)))).toBeTruthy();
    expect(screen.getByText(/A narrow question can show single entries\./)).toBeTruthy();
  });
});
