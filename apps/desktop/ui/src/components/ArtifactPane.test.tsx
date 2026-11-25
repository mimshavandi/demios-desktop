import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ArtifactPane, NO_ARTIFACT_TOOLS } from "./ArtifactPane";

afterEach(cleanup);

const props = {
  artifacts: [],
  detail: null,
  frameUrl: "http://localhost:4175/frame/",
  onOpen: () => {},
  onRestore: () => {},
  onFix: () => {},
  modelName: (r: string) => r,
  onClose: () => {},
};

describe("ArtifactPane", () => {
  it("says when the chat model can't make artifacts", () => {
    render(<ArtifactPane {...props} notice={NO_ARTIFACT_TOOLS} />);
    expect(screen.getByTestId("artifacts-notice").textContent).toContain(
      "This model can't make artifacts: it doesn't support tool calls.",
    );
  });

  it("shows no notice for a model with tool calling", () => {
    render(<ArtifactPane {...props} notice={null} />);
    expect(screen.queryByTestId("artifacts-notice")).toBeNull();
  });
});
