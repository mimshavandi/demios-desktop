import mermaid from "mermaid";
import { readSource, showError } from "./source";

// dagre layout: mermaid 12 defaults to ELK, which is stubbed out of the bundle (licence, see build.mjs).
mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: "default", layout: "dagre" });

const root = document.getElementById("root");
mermaid
  .render("dn-mermaid", readSource())
  .then(({ svg }) => {
    if (root) root.innerHTML = svg;
  })
  .catch((e: unknown) => showError("Mermaid diagram has an error", e));
