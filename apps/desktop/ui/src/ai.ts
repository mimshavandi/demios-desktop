// AI disclosure (T-STORE-09, readiness audit S-11; EU AI Act Art. 50). One place for the
// words and for the machine-readable "AI-generated" marker that model-made content carries.
//
// Stable API: artifacts (W-artifacts, T-ART-06) and exports use `aiGeneratorMeta`,
// `aiGeneratorComment` and `aiFrontMatter`. Change their names, parameters or output format
// only together with those callers. `model` is the `ModelRef` string of the message that
// made the content (`local:<id>`, `byok:<provider>:<model>`), or null when unknown.

/** The persistent line by the composer (screen-reader readable via aria-describedby). */
export const AI_DISCLOSURE = "Answers come from the AI model you chose and can be wrong. Check important facts.";

/** Shown once on first run, until the user dismisses it. */
export const AI_FIRST_RUN_NOTICE =
  "Almend lets you chat with AI models you choose: one running on this computer, or a provider you add a key for. Their answers can be wrong or made up, so check anything important.";

/** Escapes text for an HTML attribute value. */
function attr(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** What the marker names: the model ref (`local:<id>` / `byok:<provider>:<model>`), if known. */
function generatorText(model: string | null): string {
  return model === null ? "Almend (AI-generated)" : `Almend (AI-generated with ${model})`;
}

/**
 * `<meta name="generator">` for HTML and SVG-in-HTML artifacts and exports made by a model.
 * Insert it into the document head; it is the machine-readable marking (Art. 50(2)).
 */
export function aiGeneratorMeta(model: string | null): string {
  return `<meta name="generator" content="${attr(generatorText(model))}">`;
}

/**
 * Comment form of the marker for SVG or other XML artifacts. XML comments may not contain
 * "--", so every hyphen followed by another gets a space after it ("-->" can't appear).
 */
export function aiGeneratorComment(model: string | null): string {
  return `<!-- generator: ${generatorText(model).replace(/-(?=-)/g, "- ")} -->`;
}

/** Front matter for Markdown exports of model-made text. */
export function aiFrontMatter(model: string | null, createdAtMs: number): string {
  const lines = ["---", "generator: Almend", "ai_generated: true"];
  if (model !== null) lines.push(`model: ${JSON.stringify(model)}`);
  lines.push(`created: ${new Date(createdAtMs).toISOString()}`, "---", "");
  return lines.join("\n");
}
