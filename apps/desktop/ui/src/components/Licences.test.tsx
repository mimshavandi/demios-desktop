import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { Licences, loadLicences, type LicenceData } from "./Licences";

afterEach(cleanup);

const data: LicenceData = {
  packages: [
    { name: "llama.cpp and ggml", version: "vendored", source: "bundled", licence: "MIT", texts: [0] },
    { name: "tokio", version: "1.0.0", source: "rust", licence: "MIT", texts: [0] },
    { name: "dompurify", version: "3.0.0", source: "npm", licence: "(MPL-2.0 OR Apache-2.0)", texts: [1] },
  ],
  texts: ["MIT License text", "Apache License text"],
};

describe("Licences", () => {
  it("lists packages, searches, shows a text and goes back", async () => {
    render(<Licences load={async () => data} />);
    fireEvent.click(screen.getByRole("button", { name: "Show licences" }));
    expect(await screen.findByRole("button", { name: /tokio/ })).toBeTruthy();
    expect(screen.getByRole("status").textContent).toBe("3 of 3 packages");

    fireEvent.change(screen.getByLabelText("Search packages"), { target: { value: "apache" } });
    expect(screen.queryByRole("button", { name: /tokio/ })).toBeNull();
    const dp = screen.getByRole("button", { name: /dompurify/ });

    fireEvent.click(dp);
    expect(await screen.findByText("Apache License text")).toBeTruthy();
    expect(screen.getByText(/npm package · \(MPL-2.0 OR Apache-2.0\)/)).toBeTruthy();
    await waitFor(() => expect(document.activeElement?.textContent).toBe("dompurify 3.0.0"));

    fireEvent.click(screen.getByRole("button", { name: "All packages" }));
    expect(await screen.findByRole("button", { name: /dompurify/ })).toBeTruthy();

    fireEvent.change(screen.getByLabelText("Search packages"), { target: { value: "nothing-like-this" } });
    expect(screen.getByText(/No package matches/)).toBeTruthy();
  });

  it("says when the build has no list, and retries after an error", async () => {
    render(<Licences load={async () => null} />);
    fireEvent.click(screen.getByRole("button", { name: "Show licences" }));
    expect(await screen.findByText(/doesn't include the licence list/)).toBeTruthy();
    cleanup();

    let fail = true;
    render(
      <Licences
        load={async () => {
          if (fail) throw new Error("boom");
          return data;
        }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Show licences" }));
    expect(await screen.findByText(/Couldn't load the licence list/)).toBeTruthy();
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("button", { name: /tokio/ })).toBeTruthy();
  });

  it("loads the bundled module; no module or a wrong shape means missing", async () => {
    const mod = (d: unknown) => ({ "../generated/licenses.json": async () => ({ default: d }) });
    expect(await loadLicences({})).toBeNull();
    expect(await loadLicences(mod({ html: true }))).toBeNull();
    expect(await loadLicences(mod(null))).toBeNull();
    expect((await loadLicences(mod(data)))?.packages.length).toBe(3);
  });

  it("hides the list and returns focus to Show licences", async () => {
    render(<Licences load={async () => data} />);
    fireEvent.click(screen.getByRole("button", { name: "Show licences" }));
    await screen.findByRole("button", { name: /tokio/ });
    expect(screen.getByRole("status").textContent).toBe("3 of 3 packages");
    fireEvent.click(screen.getByRole("button", { name: "Hide" }));
    await waitFor(() => expect(document.activeElement?.textContent).toBe("Show licences"));
  });
});
