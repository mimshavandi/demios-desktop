// Artifact pane chrome (docs/design/design-system.md §4 "Artifact pane chrome"): title, type,
// version switcher with author and time, restore, the sandboxed frame, and the states
// loading (render skeleton) · ready · runtime error (error bar + "Ask to fix") · network blocked
// (inline notice + per-artifact toggle). Framework-agnostic DOM; styled only with `--dn-*`
// tokens from ui/design-tokens (the host loads tokens.css). Styles go in through a constructable
// stylesheet, so the host CSP needs no 'unsafe-inline'.

import { ArtifactFrame } from "./frame";
import { latest, type Artifact, type ArtifactVersion, type SandboxIssue } from "./types";

export interface FixRequest {
  artifactId: string;
  version: number;
  issues: SandboxIssue[];
}

export interface ArtifactViewOptions {
  /** Frame shell URL on the artifact origin (see ArtifactFrameOptions.frameUrl). */
  frameUrl: string;
  /** "Restore this version": the host calls the store's rollback, then `setArtifact`. */
  onRestore?: (artifactId: string, n: number) => void;
  /** "Ask to fix": send the problems to the model. */
  onFix?: (req: FixRequest) => void;
  /** Per-artifact network toggle (ADR-0004); off by default. */
  allowNetwork?: (artifactId: string) => boolean;
  /** "Allow network for this artifact" in the blocked-network notice. The view re-renders after. */
  onAllowNetwork?: (artifactId: string) => void;
  maxHeight?: number;
}

const CSS = `
.dn-av { display: flex; flex-direction: column; min-width: 0; border: 1px solid var(--dn-color-border); border-radius: var(--dn-radius-lg); overflow: hidden; background: var(--dn-color-surface); color: var(--dn-color-text); font-family: var(--dn-font-sans); font-size: var(--dn-text-base); line-height: var(--dn-leading-base); box-shadow: var(--dn-shadow-1); }
.dn-av-bar { display: flex; align-items: center; gap: var(--dn-space-2); padding: var(--dn-space-2) var(--dn-space-3); border-bottom: 1px solid var(--dn-color-border); flex-wrap: wrap; }
.dn-av-title { margin: 0; font-size: inherit; font-weight: var(--dn-weight-semibold); flex: 1 1 auto; min-width: 8em; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dn-av-kind { font-size: var(--dn-text-xs); line-height: var(--dn-leading-xs); color: var(--dn-color-text-muted); background: var(--dn-color-surface-sunken); border-radius: var(--dn-radius-full); padding: 0 var(--dn-space-2); }
.dn-av-versions { display: flex; align-items: center; gap: var(--dn-space-1); }
.dn-av button, .dn-av select { font: inherit; font-size: var(--dn-text-sm); min-height: var(--dn-hit-target); padding: 0 var(--dn-space-3); border-radius: var(--dn-radius-md); border: 1px solid var(--dn-color-border-strong); background: var(--dn-color-surface); color: var(--dn-color-text); cursor: pointer; transition: background var(--dn-motion-fast) var(--dn-motion-ease); }
.dn-av button:hover:not(:disabled), .dn-av select:hover { background: var(--dn-color-surface-sunken); }
.dn-av button:disabled { color: var(--dn-color-text-subtle); cursor: default; }
.dn-av button:focus-visible, .dn-av select:focus-visible { outline: 2px solid var(--dn-color-focus); outline-offset: 2px; }
.dn-av-icon { min-width: var(--dn-hit-target); padding: 0 !important; }
.dn-av button.dn-av-primary { background: var(--dn-color-accent); border-color: var(--dn-color-accent); color: var(--dn-color-text-on-accent); }
.dn-av button.dn-av-primary:hover:not(:disabled) { background: var(--dn-color-accent-hover); }
.dn-av-notice { display: flex; gap: var(--dn-space-3); align-items: center; justify-content: space-between; flex-wrap: wrap; padding: var(--dn-space-2) var(--dn-space-3); font-size: var(--dn-text-sm); line-height: var(--dn-leading-sm); border-bottom: 1px solid var(--dn-color-border); }
.dn-av-old { background: var(--dn-color-accent-soft); color: var(--dn-color-accent-text); }
.dn-av-net { background: var(--dn-color-warning-soft); color: var(--dn-color-text); }
.dn-av-error { background: var(--dn-color-danger-soft); color: var(--dn-color-text); border-top: 1px solid var(--dn-color-border); border-bottom: 0; }
.dn-av-error-text { flex: 1 1 16em; min-width: 0; }
.dn-av-error-text strong { color: var(--dn-color-danger); font-weight: var(--dn-weight-semibold); }
.dn-av-error details { width: 100%; }
.dn-av-error summary { cursor: pointer; color: var(--dn-color-text-muted); }
.dn-av-error ul { margin: var(--dn-space-1) 0 0; padding-left: var(--dn-space-5); max-height: 8em; overflow: auto; font-family: var(--dn-font-mono); font-size: var(--dn-text-xs); line-height: var(--dn-leading-xs); }
.dn-av-body { position: relative; background: var(--dn-color-surface); }
.dn-av-skeleton { position: absolute; inset: 0; padding: var(--dn-space-4); display: grid; gap: var(--dn-space-3); align-content: start; background: var(--dn-color-surface); }
.dn-av-skeleton span { display: block; height: var(--dn-space-3); border-radius: var(--dn-radius-sm); background: var(--dn-color-surface-sunken); }
.dn-av-skeleton span:nth-child(2) { width: 80%; } .dn-av-skeleton span:nth-child(3) { width: 60%; }
.dn-av [hidden] { display: none !important; }
`;

let sheetInstalled = false;
function installStyles(): void {
  if (sheetInstalled) return;
  sheetInstalled = true;
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(CSS);
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text !== undefined) e.textContent = text;
  return e;
}

const TYPE_LABEL: Record<string, string> = { html: "HTML", react: "React", svg: "SVG", mermaid: "Diagram", markdown: "Document" };

function timeOf(rfc3339: string): string {
  const d = new Date(rfc3339);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** "v3 — restored from v1 · you · 11:42" (design standard §4). */
export function versionLabel(v: ArtifactVersion): string {
  const restored = v.parent_n !== null && v.parent_n !== v.n - 1 ? ` — restored from v${v.parent_n}` : "";
  const who = v.author === "user" ? "you" : "model";
  const at = timeOf(v.created_at);
  return `v${v.n}${restored} · ${who}${at ? ` · ${at}` : ""}`;
}

export class ArtifactView {
  readonly element: HTMLElement;
  private artifact: Artifact | null = null;
  private shown = 0;
  private issues: SandboxIssue[] = [];
  private networkBlocked = false;
  private readonly frame: ArtifactFrame;
  private readonly opts: ArtifactViewOptions;
  private readonly ui: {
    title: HTMLElement;
    kind: HTMLElement;
    prev: HTMLButtonElement;
    next: HTMLButtonElement;
    select: HTMLSelectElement;
    old: HTMLElement;
    oldText: HTMLElement;
    restore: HTMLButtonElement;
    net: HTMLElement;
    allowNet: HTMLButtonElement;
    skeleton: HTMLElement;
    error: HTMLElement;
    errorFirst: HTMLElement;
    errorMore: HTMLElement;
    issueList: HTMLUListElement;
    fix: HTMLButtonElement;
  };

  constructor(container: HTMLElement, opts: ArtifactViewOptions) {
    installStyles();
    this.opts = opts;
    this.element = el("section", { class: "dn-av", "aria-label": "Artifact" });

    const bar = el("div", { class: "dn-av-bar" });
    const title = el("h2", { class: "dn-av-title", "data-testid": "artifact-title" });
    const kind = el("span", { class: "dn-av-kind", "data-testid": "artifact-kind" });
    const versions = el("div", { class: "dn-av-versions" });
    const prev = el("button", { type: "button", class: "dn-av-icon", "aria-label": "Previous version", title: "Previous version", "data-testid": "version-prev" }, "‹");
    const select = el("select", { "aria-label": "Version", "data-testid": "version-select" });
    const next = el("button", { type: "button", class: "dn-av-icon", "aria-label": "Next version", title: "Next version", "data-testid": "version-next" }, "›");
    versions.append(prev, select, next);
    bar.append(title, kind, versions);

    const old = el("div", { class: "dn-av-notice dn-av-old", "data-testid": "old-version-banner" });
    const oldText = el("span");
    const restore = el("button", { type: "button", "data-testid": "version-restore" }, "Restore this version");
    old.append(oldText, restore);
    old.hidden = true;

    const net = el("div", { class: "dn-av-notice dn-av-net", role: "status", "data-testid": "network-blocked" });
    const netText = el("span", {}, "This artifact tried to use the network, which is off for artifacts.");
    const allowNet = el("button", { type: "button", "data-testid": "network-allow" }, "Allow network for this artifact");
    net.append(netText, allowNet);
    net.hidden = true;

    const body = el("div", { class: "dn-av-body" });
    const skeleton = el("div", { class: "dn-av-skeleton", "aria-hidden": "true", "data-testid": "artifact-loading" });
    skeleton.append(el("span"), el("span"), el("span"));

    const error = el("div", { class: "dn-av-notice dn-av-error", role: "alert", "data-testid": "artifact-issues" });
    const errorText = el("div", { class: "dn-av-error-text" });
    const errorFirst = el("span");
    errorText.append(el("strong", {}, "Error "), errorFirst);
    const fix = el("button", { type: "button", class: "dn-av-primary", "data-testid": "artifact-fix" }, "Ask to fix");
    const details = el("details");
    const errorMore = el("summary");
    const issueList = el("ul");
    details.append(errorMore, issueList);
    error.append(errorText, fix, details);
    error.hidden = true;

    this.element.append(bar, old, net, body, error);
    container.appendChild(this.element);

    this.frame = new ArtifactFrame(body, {
      frameUrl: opts.frameUrl,
      maxHeight: opts.maxHeight ?? 1200,
      onIssue: (i) => this.addIssue(i),
      onReady: () => {
        skeleton.hidden = true;
        body.removeAttribute("aria-busy");
      },
    });
    body.appendChild(skeleton);
    this.ui = { title, kind, prev, next, select, old, oldText, restore, net, allowNet, skeleton, error, errorFirst, errorMore, issueList, fix };

    prev.addEventListener("click", () => void this.show(this.shown - 1));
    next.addEventListener("click", () => void this.show(this.shown + 1));
    select.addEventListener("change", () => void this.show(Number(select.value)));
    restore.addEventListener("click", () => {
      if (this.artifact) this.opts.onRestore?.(this.artifact.id, this.shown);
    });
    fix.addEventListener("click", () => {
      if (this.artifact && this.issues.length) {
        this.opts.onFix?.({ artifactId: this.artifact.id, version: this.shown, issues: [...this.issues] });
      }
    });
    allowNet.addEventListener("click", () => {
      if (!this.artifact) return;
      this.opts.onAllowNetwork?.(this.artifact.id);
      void this.show(this.shown);
    });
  }

  /** Show an artifact (or a newer copy of the same one); jumps to its latest version. */
  setArtifact(a: Artifact): Promise<void> {
    this.artifact = a;
    return this.show(latest(a).n);
  }

  /** The iframe currently shown (for tests and the embedding shell). */
  get iframe(): HTMLIFrameElement | null {
    return this.frame.frame;
  }

  get shownVersion(): number {
    return this.shown;
  }

  destroy(): void {
    this.frame.destroy();
    this.element.remove();
  }

  private version(n: number): ArtifactVersion | undefined {
    return this.artifact?.versions.find((v) => v.n === n);
  }

  private async show(n: number): Promise<void> {
    const a = this.artifact;
    const v = this.version(n);
    if (!a || !v) return;
    this.shown = n;
    const newest = latest(a).n;
    const t = a.artifact_type;
    this.ui.title.textContent = a.title;
    this.ui.kind.textContent = t.kind === "code" ? t.language : (TYPE_LABEL[t.kind] ?? t.kind);
    this.ui.select.replaceChildren(
      ...[...a.versions].reverse().map((x) => {
        const o = el("option", { value: String(x.n) }, versionLabel(x));
        o.selected = x.n === n;
        return o;
      }),
    );
    const first = a.versions[0]?.n ?? 1;
    this.ui.prev.disabled = n <= first;
    this.ui.next.disabled = n >= newest;
    this.ui.old.hidden = n === newest;
    this.ui.oldText.textContent = `Viewing v${n}. The latest is v${newest}.`;
    this.ui.restore.hidden = !this.opts.onRestore;
    this.issues = [];
    this.networkBlocked = false;
    this.renderIssues();
    this.ui.skeleton.hidden = false;
    this.ui.skeleton.parentElement?.setAttribute("aria-busy", "true");
    await this.frame.render(t, v.content, {
      title: a.title,
      allowNetwork: this.opts.allowNetwork?.(a.id) ?? false,
    });
  }

  private addIssue(i: SandboxIssue): void {
    if (i.kind === "network") {
      this.networkBlocked = true;
      this.renderIssues();
      return;
    }
    if (this.issues.length >= 50) return;
    this.issues.push(i);
    this.renderIssues();
  }

  private renderIssues(): void {
    const n = this.issues.length;
    const allowed = this.artifact ? (this.opts.allowNetwork?.(this.artifact.id) ?? false) : false;
    this.ui.net.hidden = !this.networkBlocked || allowed;
    this.ui.allowNet.hidden = !this.opts.onAllowNetwork;
    this.ui.error.hidden = n === 0;
    const firstIssue = this.issues[0];
    this.ui.errorFirst.textContent = firstIssue ? firstIssue.message : "";
    this.ui.errorMore.textContent = n === 1 ? "Details" : `Details (${n} problems)`;
    this.ui.fix.hidden = !this.opts.onFix;
    this.ui.issueList.replaceChildren(...this.issues.map((i) => el("li", {}, `${i.kind}: ${i.message}${i.detail ? `\n${i.detail}` : ""}`)));
  }
}
