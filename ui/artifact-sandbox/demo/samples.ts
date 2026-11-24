import type { ArtifactType } from "../src/index";

export const SAMPLES: Record<string, { type: ArtifactType; title: string; content: string }> = {
  react: {
    type: { kind: "react" },
    title: "Habit tracker",
    content: `import { useState } from "react";

type Habit = { name: string; done: boolean[] };
const DAYS = ["M", "T", "W", "T", "F", "S", "S"];

export default function App() {
  const [habits, setHabits] = useState<Habit[]>([
    { name: "Read 20 min", done: [true, true, false, true, false, false, false] },
    { name: "Walk", done: [true, false, true, true, true, false, false] },
  ]);
  const toggle = (h: number, d: number) =>
    setHabits((hs) => hs.map((x, i) => (i === h ? { ...x, done: x.done.map((v, j) => (j === d ? !v : v)) } : x)));
  return (
    <div style={{ fontFamily: "system-ui", padding: 20 }}>
      <h2 style={{ marginTop: 0 }}>This week</h2>
      <table style={{ borderCollapse: "collapse" }}>
        <thead><tr><th />{DAYS.map((d, i) => <th key={i} style={{ width: 32 }}>{d}</th>)}</tr></thead>
        <tbody>
          {habits.map((h, i) => (
            <tr key={h.name}>
              <td style={{ paddingRight: 12 }}>{h.name}</td>
              {h.done.map((v, j) => (
                <td key={j} style={{ textAlign: "center" }}>
                  <button aria-label={h.name + " " + DAYS[j]} onClick={() => toggle(i, j)}
                    style={{ width: 26, height: 26, borderRadius: 13, border: "1px solid #888", background: v ? "#2a9d8f" : "transparent" }} />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      <p data-testid="react-total">{habits.reduce((n, h) => n + h.done.filter(Boolean).length, 0)} check-ins</p>
    </div>
  );
}
`,
  },
  mermaid: {
    type: { kind: "mermaid" },
    title: "Artifact flow",
    content: `flowchart LR
  M[Model] -->|artifact_create| S[(Store v1)]
  M -->|artifact_update| S2[(v2)]
  U[User] -->|restore v1| S3[(v3, parent v1)]
  S --> R[Sandbox frame]
  S2 --> R
  S3 --> R`,
  },
  markdown: {
    type: { kind: "markdown" },
    title: "Release notes",
    content: `# Release notes

- **Artifacts** render in a sandbox with *no network*.
- Versions are append-only; rollback creates a new version.

| Type | Renderer |
|---|---|
| html | iframe |
| react | sucrase + vendored React |

\`\`\`rust
let v = store.rollback(id, 1, &ctx)?;
\`\`\`
`,
  },
  svg: {
    type: { kind: "svg" },
    title: "Badge",
    content: `<svg xmlns="http://www.w3.org/2000/svg" width="240" height="120" viewBox="0 0 240 120">
  <rect x="4" y="4" width="232" height="112" rx="18" fill="#264653"/>
  <circle cx="60" cy="60" r="30" fill="#e9c46a"/>
  <text x="104" y="68" font-family="system-ui" font-size="22" fill="#fff">sandboxed</text>
</svg>`,
  },
  code: {
    type: { kind: "code", language: "python" },
    title: "fib.py",
    content: `def fib(n: int) -> int:
    a, b = 0, 1
    for _ in range(n):
        a, b = b, a + b
    return a


print([fib(i) for i in range(10)])
`,
  },
};
