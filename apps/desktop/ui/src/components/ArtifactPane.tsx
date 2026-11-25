// Right-hand artifact pane (ADR-0004, T-ART-06; design standard §4). The renderer and its
// chrome (version switcher, restore, error bar + Ask to fix, network notice) come from
// ui/artifact-sandbox; this component lists the chat's artifacts and mounts that view.
// The sandbox package is loaded only when an artifact is shown (its renderers are large).

import { Code2, FileText, Image, LayoutTemplate, Network, Sparkles } from "lucide-react";
import { useEffect, useRef, useState, type ReactElement } from "react";
import type { ArtifactView as SandboxView, Artifact as SandboxArtifact, KeyPress } from "@dn-artifacts";
import { ArtifactWebviewFrame } from "../artifactWebview";
import { isMac, usesArtifactWebview } from "../platform";
import type { ArtifactDetail, ArtifactSummary, ArtifactType, ArtifactVersionView, Id } from "../ipc/types";
import { Button, ICON, InlineMessage } from "./primitives";

export interface ArtifactPaneProps {
  artifacts: ArtifactSummary[];
  /** The artifact shown, with every version; `null` while none is open (or loading). */
  detail: ArtifactDetail | null;
  /** Frame shell on the artifact origin (see `artifactFrameUrl`). */
  frameUrl: string;
  onOpen: (id: Id) => void;
  onRestore: (id: Id, n: number) => void;
  /** "Ask to fix": the prompt is sent as the user's next message. */
  onFix: (prompt: string) => void;
  /** Display name of a model ref, for "Made by <model>". */
  modelName: (ref: string) => string;
  /** Content as rendered, with the AI-generated marker; stored versions stay unchanged. */
  mark?: (type: ArtifactType, version: ArtifactVersionView) => string;
  onClose: () => void;
  /** How the chat model changes artifacts, for the "Ask to fix" wording. */
  fixMode?: "tools" | "fenced";
}

export function typeLabel(t: ArtifactType): string {
  switch (t.kind) {
    case "html":
      return "HTML";
    case "react":
      return "React";
    case "svg":
      return "SVG";
    case "mermaid":
      return "Diagram";
    case "markdown":
      return "Document";
    case "code":
      return t.language;
  }
}

export function TypeIcon({ type }: { type: ArtifactType }): ReactElement {
  const Icon =
    type.kind === "markdown"
      ? FileText
      : type.kind === "svg"
        ? Image
        : type.kind === "mermaid"
          ? Network
          : type.kind === "code"
            ? Code2
            : LayoutTemplate;
  return <Icon {...ICON} aria-hidden="true" />;
}

/**
 * App shortcuts (App.tsx, design-system §7) that still work while the artifact has focus: new
 * chat, settings, sidebar. Artifact code can post a "key" report without any key being pressed,
 * so only shortcuts that just move around the app are passed on. Stop (⌘/Ctrl+.) and copy last
 * answer (⌘/Ctrl+Shift+C) are not: a forged report could cancel every answer or overwrite the
 * clipboard.
 */
export const FRAME_SHORTCUTS: ReadonlySet<string> = new Set(["n", ",", "\\"]);

/** Forwarded shortcuts closer together than this are dropped (a forged report can't loop one). */
export const FRAME_SHORTCUT_GAP_MS = 500;

/** What a key pressed in the frame does: close the pane, run an app shortcut, or nothing. */
export function frameKeyAction(k: KeyPress, mac = isMac): "close" | "shortcut" | null {
  if (k.key === "Escape" && !k.ctrl && !k.meta && !k.alt) return "close";
  const mod = mac ? k.meta : k.ctrl;
  return mod && !k.alt && !k.shift && FRAME_SHORTCUTS.has(k.key.toLowerCase()) ? "shortcut" : null;
}

/** `frameKeyAction` with a rate limit on shortcuts (Escape only closes, so it isn't limited). */
export function frameKeyGate(now: () => number = () => Date.now()): (k: KeyPress, mac?: boolean) => "close" | "shortcut" | null {
  let last = Number.NEGATIVE_INFINITY;
  return (k, mac = isMac) => {
    const action = frameKeyAction(k, mac);
    if (action !== "shortcut") return action;
    const t = now();
    if (t - last < FRAME_SHORTCUT_GAP_MS) return null;
    last = t;
    return action;
  };
}

type Sandbox = typeof import("@dn-artifacts");
let sandbox: Promise<Sandbox> | null = null;
function loadSandbox(): Promise<Sandbox> {
  sandbox ??= import("@dn-artifacts");
  return sandbox;
}

export function ArtifactPane(p: ArtifactPaneProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<SandboxView | null>(null);
  const [viewReady, setViewReady] = useState(0);
  const [loadError, setLoadError] = useState<string | null>(null);
  // Per-artifact network permission: this session only, never stored.
  const network = useRef<Set<Id>>(new Set());
  // The view keeps the callbacks it was built with; route them through a ref.
  const latest = useRef(p);
  latest.current = p;
  const hasDetail = p.detail !== null;

  useEffect(() => {
    if (!hasDetail) return;
    let dead = false;
    let view: SandboxView | null = null;
    const keyGate = frameKeyGate();
    loadSandbox()
      .then((m) => {
        if (dead || hostRef.current === null) return;
        view = new m.ArtifactView(hostRef.current, {
          frameUrl: latest.current.frameUrl,
          onRestore: (id, n) => latest.current.onRestore(id, n),
          onFix: (req) => latest.current.onFix(req.prompt),
          allowNetwork: (id) => network.current.has(id),
          onAllowNetwork: (id) => network.current.add(id),
          madeBy: (v) => {
            if (v.author !== "model") return null;
            const model = (v as ArtifactVersionView).model;
            return model === null ? "an AI model" : latest.current.modelName(model);
          },
          fixMode: () => latest.current.fixMode ?? "tools",
          mark: (a, v) =>
            v.author === "model"
              ? (latest.current.mark?.(a.artifact_type, v as ArtifactVersionView) ?? v.content)
              : v.content,
          onKey: (k) => {
            const action = keyGate(k);
            if (action === "close") latest.current.onClose();
            else if (action === "shortcut") {
              window.dispatchEvent(
                new KeyboardEvent("keydown", { key: k.key, ctrlKey: k.ctrl, metaKey: k.meta, altKey: k.alt, shiftKey: k.shift, bubbles: true, cancelable: true }),
              );
            }
          },
          // Windows: the frame is in a separate webview (ADR-0012).
          ...(usesArtifactWebview && { createFrame: (container, opts) => new ArtifactWebviewFrame(m, container, opts) }),
        });
        viewRef.current = view;
        setLoadError(null);
        setViewReady((n) => n + 1);
      })
      .catch((e: unknown) => {
        console.error("artifact renderer failed to load", e);
        sandbox = null;
        if (!dead) setLoadError("The artifact viewer couldn't load.");
      });
    return () => {
      dead = true;
      view?.destroy();
      viewRef.current = null;
    };
  }, [hasDetail]);

  // Network permission is for the version the user saw: a newer version the model wrote
  // starts with the network off again (the toggle asks again).
  const seen = useRef<Map<Id, number>>(new Map());
  useEffect(() => {
    const d = p.detail;
    if (d === null) return;
    const last = d.versions.at(-1);
    const before = seen.current.get(d.id);
    if (last !== undefined && before !== undefined && last.n > before && last.author === "model") {
      network.current.delete(d.id);
    }
    if (last !== undefined) seen.current.set(d.id, last.n);
  }, [p.detail]);

  useEffect(() => {
    if (viewRef.current !== null && p.detail !== null) {
      void viewRef.current.setArtifact(p.detail as SandboxArtifact);
    }
  }, [p.detail, viewReady]);

  return (
    <aside
      className="artifacts"
      aria-label="Artifacts"
      data-testid="artifact-pane"
      onKeyDown={(e) => {
        if (e.key === "Escape") p.onClose();
      }}
    >
      {p.artifacts.length === 0 ? (
        <div className="dn-empty">
          <Sparkles {...ICON} aria-hidden="true" />
          <h2>No artifact open</h2>
          <p>Documents, code and small apps the model makes will open here.</p>
        </div>
      ) : (
        <div className="artifact-pane-body">
          {p.artifacts.length > 1 && (
            <nav className="artifact-list" aria-label="Artifacts in this chat">
              {p.artifacts.map((a) => (
                <button
                  key={a.id}
                  type="button"
                  className={`artifact-tab${a.id === p.detail?.id ? " selected" : ""}`}
                  aria-current={a.id === p.detail?.id ? "true" : undefined}
                  onClick={() => p.onOpen(a.id)}
                  data-testid="artifact-tab"
                >
                  <TypeIcon type={a.artifact_type} />
                  <span>{a.title}</span>
                </button>
              ))}
            </nav>
          )}
          {loadError !== null && <InlineMessage tone="error">{loadError}</InlineMessage>}
          <div
            ref={hostRef}
            className={`artifact-host${usesArtifactWebview ? " artifact-host--webview" : ""}`}
            hidden={!hasDetail}
            data-testid="artifact-host"
          />
        </div>
      )}
    </aside>
  );
}

/** Artifact card in a message (design standard §4): title, type icon, version, "Open". */
export function ArtifactCard({
  title,
  type,
  version,
  onOpen,
}: {
  title: string;
  type: ArtifactType;
  version: number;
  onOpen: () => void;
}) {
  return (
    <div className="artifact-card" data-testid="artifact-card">
      <span className="artifact-card-icon">
        <TypeIcon type={type} />
      </span>
      <span className="artifact-card-text">
        <span className="artifact-card-title">{title}</span>
        <span className="artifact-card-meta">
          {typeLabel(type)} · v{version}
        </span>
      </span>
      <Button size="sm" onClick={onOpen} data-testid="artifact-card-open" aria-label={`Open ${title}`}>
        Open
      </Button>
    </div>
  );
}
