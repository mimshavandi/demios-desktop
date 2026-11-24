// Right-hand pane reserved for artifacts (ADR-0004). W-artifacts mounts the sandboxed
// renderer here; the shell only provides the slot.

export function ArtifactPane() {
  return (
    <aside className="artifacts" aria-label="Artifacts" data-testid="artifact-pane">
      <div className="artifacts-empty">
        <h2>Artifacts</h2>
        <p>Documents, code and apps the model creates will open here.</p>
      </div>
    </aside>
  );
}
