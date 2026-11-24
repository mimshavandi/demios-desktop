import { marked } from "marked";
import { readSource, showError } from "./source";

try {
  const root = document.getElementById("root");
  if (!root) throw new Error("missing #root");
  // Raw HTML in Markdown is kept: it runs in the same sandbox as an HTML artifact would.
  root.innerHTML = marked.parse(readSource(), { async: false, gfm: true });
} catch (e) {
  showError("Markdown failed to render", e);
}
