// How much a provider's or server's model reads at once (ADR-0020 §2.1). Design:
// ContextSize (Design System, approved 2026-11-27). Demios never guesses the size: when
// nobody knows it, the person sets it here.

import { TriangleAlert } from "lucide-react";
import { createContext, useContext, useEffect, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import { errorMessage, type ModelContext } from "../ipc/types";
import { Badge, Button, ICON, TextField } from "./primitives";

const MIN = 1024;
const MAX = 10_000_000;
const INVALID = "Enter a whole number of tokens, from 1,024 to 10,000,000.";

const fmt = (n: number) => n.toLocaleString("en-US");

/** Where Settings › Models was opened for (a chat's "Set context size"), and who to tell
 * when a size is saved (the chat then offers Retry). */
export const ContextSizeTarget = createContext<{
  focus: string | null;
  onSaved: (modelRef: string) => void;
}>({ focus: null, onSaved: () => {} });

/** The whole number in `text`, or null when it isn't one in range. */
export function parseSize(text: string): number | null {
  const t = text.trim().replace(/[,\s]/g, "");
  if (!/^\d+$/.test(t)) return null;
  const n = Number(t);
  return n >= MIN && n <= MAX ? n : null;
}

/** Asks for the size only once the row is on screen: a provider may answer per model. */
function useVisible(ref: React.RefObject<HTMLElement | null>): boolean {
  const [visible, setVisible] = useState(typeof IntersectionObserver === "undefined");
  useEffect(() => {
    const el = ref.current;
    if (visible || el === null || typeof IntersectionObserver === "undefined") return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) setVisible(true);
    });
    io.observe(el);
    return () => io.disconnect();
  }, [ref, visible]);
  return visible;
}

export function ContextSize({
  api,
  modelRef,
  name,
  provider,
}: {
  api: Api;
  modelRef: string;
  /** The model's name, as the row shows it. */
  name: string;
  /** Who reports sizes for this model ("Anthropic", a server's name). */
  provider: string;
}) {
  const target = useContext(ContextSizeTarget);
  const focus = target.focus === modelRef;
  const box = useRef<HTMLDivElement>(null);
  const visible = useVisible(box);
  const [ctx, setCtx] = useState<ModelContext | null>(null);
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!visible) return;
    let alive = true;
    api
      .modelContext(modelRef)
      .then((c) => alive && setCtx(c))
      .catch(() => alive && setCtx({ tokens: null, source: null }));
    return () => {
      alive = false;
    };
  }, [api, modelRef, visible]);

  const unknown = ctx !== null && ctx.tokens === null;
  const showField = unknown || editing;
  useEffect(() => {
    if (focus && showField) input.current?.focus();
  }, [focus, showField]);

  if (ctx === null) return <div ref={box} />;

  const save = async () => {
    const n = parseSize(text);
    if (n === null) {
      setError(INVALID);
      return;
    }
    setSaving(true);
    try {
      const c = await api.setModelContextSize(modelRef, n);
      setCtx(c);
      setEditing(false);
      setError(null);
      target.onSaved(modelRef);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSaving(false);
    }
  };

  if (!showField && ctx.tokens !== null) {
    const from =
      ctx.source === "user"
        ? "you set this"
        : ctx.source === "published"
          ? "from Demios' list of published sizes"
          : `from ${provider}`;
    return (
      <div ref={box} className="context-size">
        <span className="dn-help">
          Reads up to {fmt(ctx.tokens)} tokens at once ({from}).
        </span>
        {ctx.source === "user" && (
          <Button
            variant="ghost"
            size="sm"
            aria-label={`Change context size for ${name}`}
            onClick={() => {
              setText(String(ctx.tokens));
              setEditing(true);
            }}
          >
            Change
          </Button>
        )}
      </div>
    );
  }

  return (
    <div ref={box} className="context-size">
      {unknown && (
        <Badge tone="warning" icon={<TriangleAlert {...ICON} aria-hidden="true" />}>
          Context size needed
        </Badge>
      )}
      <form
        className="context-size-form"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <TextField
          ref={input}
          label="Context size"
          inputMode="numeric"
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            setError(null);
          }}
          error={error}
          help="In tokens. Find it in the provider's documentation for this model. Demios uses it to fit long chats into what the model can read."
        />
        <span className="context-size-unit" aria-hidden="true">
          tokens
        </span>
        <Button type="submit" loading={saving}>
          Save
        </Button>
      </form>
    </div>
  );
}
