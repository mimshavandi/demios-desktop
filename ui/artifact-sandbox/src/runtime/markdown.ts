import { marked } from "marked";
import { readSource, showError } from "./source";

/** One leading YAML front-matter block (`---` … `---`): the host's AI marker. Not shown; anything after it is the artifact's own text. */
const FRONT_MATTER = /^---\r?\n[\s\S]*?\r?\n---\r?\n/;

try {
  const root = document.getElementById("root");
  if (!root) throw new Error("missing #root");
  // Raw HTML in Markdown is kept: it runs in the same sandbox as an HTML artifact would.
  root.innerHTML = marked.parse(readSource().replace(FRONT_MATTER, ""), { async: false, gfm: true });
} catch (e) {
  showError("Markdown failed to render", e);
}
