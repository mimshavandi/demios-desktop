// Unit tests (jsdom) never load the real renderer: it needs a webview and a built
// ui/artifact-sandbox. Tests that mount the pane mock "@dn-artifacts" (see App.test.tsx).
export class ArtifactView {
  constructor() {
    throw new Error("the artifact renderer is not available in unit tests; mock @dn-artifacts");
  }
}
