// Display helper for the fenced artifact fallback (models without tool calling, ADR-0004).
// The Rust side saves closed `<artifact>` blocks from those models' text; the stored answer
// keeps the raw text (it is the model's own history). Here the saved blocks are taken out of
// the shown text, since the artifact card stands for them. Text from tool-capable models is
// never parsed for artifacts, so its blocks stay visible as written.

/** An opening tag at line start (≤ 3 spaces), up to its closing tag at line start. */
const BLOCK = /^ {0,3}<(?:ant)?artifact\b[^>\n]*>[\s\S]*?^ {0,3}<\/(?:ant)?artifact>[^\S\n]*$\n?/gim;

/** `text` without complete `<artifact>` blocks. Only call it for answers that saved artifacts. */
export function withoutArtifactBlocks(text: string): string {
  return text.replace(BLOCK, "").replace(/\n{3,}/g, "\n\n").trim();
}
