import { marked } from "marked";
import { readSource, showError } from "./source";

/** A leading YAML front-matter block (`---` … `---`), e.g. the host's AI-generated marker. Not shown. */
const FRONT_MATTER = /^---\r?\n[\s\S]*?\r?\n---\r?\n/;

try {
  const root = document.getElementById("root");
  if (!root) throw new Error("missing #root");
  // Raw HTML in Markdown is kept: it runs in the same sandbox as an HTML artifact would.
  root.innerHTML = marked.parse(readSource().replace(FRONT_MATTER, ""), { async: false, gfm: true });
} catch (e) {
  showError("Markdown failed to render", e);
}
