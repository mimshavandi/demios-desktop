// Right-hand pane reserved for artifacts (ADR-0004). W-artifacts mounts the sandboxed
// renderer here; the shell only provides the slot and its empty state.

import { Sparkles } from "lucide-react";
import { ICON } from "./primitives";

export function ArtifactPane() {
  return (
    <aside className="artifacts" aria-label="Artifacts" data-testid="artifact-pane">
      <div className="dn-empty">
        <Sparkles {...ICON} aria-hidden="true" />
        <h2>No artifact open</h2>
        <p>Documents, code and small apps the model makes will open here.</p>
      </div>
    </aside>
  );
}
