import { ArrowLeft, ScrollText } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { Button, ICON, InlineMessage, Skeleton, TextField } from "./primitives";

/** ui/src/generated/licenses.json, written by scripts/licenses.mjs (T-STORE-07). */
export interface LicenceData {
  packages: { name: string; version: string; source: "rust" | "npm" | "bundled"; licence: string; texts: number[] }[];
  texts: string[];
}

type Load =
  | { kind: "loading" }
  | { kind: "missing" }
  | { kind: "error"; message: string }
  | { kind: "ready"; data: LicenceData };

const SOURCE: Record<LicenceData["packages"][number]["source"], string> = {
  rust: "Rust crate",
  npm: "npm package",
  bundled: "Bundled C/C++",
};

// Bundled at build time as a lazily loaded chunk (no fetch: the CSP allows no connect to
// 'self'). The glob matches nothing when the file wasn't generated, e.g. in dev builds.
const generated = import.meta.glob<{ default: unknown }>("../generated/licenses.json");

export async function loadLicences(
  modules: Record<string, () => Promise<{ default: unknown }>> = generated,
): Promise<LicenceData | null> {
  const load = Object.values(modules)[0];
  if (load === undefined) return null;
  const data = (await load()).default as Partial<LicenceData> | null;
  return data !== null && Array.isArray(data?.packages) && Array.isArray(data?.texts) ? (data as LicenceData) : null;
}

/**
 * Settings › About › Open-source licences: a searchable list of every shipped dependency
 * (Rust crates, npm packages, bundled C/C++) with its full licence text.
 */
export function Licences({ load = loadLicences }: { load?: () => Promise<LicenceData | null> }) {
  const [open, setOpen] = useState(false);
  const [state, setState] = useState<Load>({ kind: "loading" });
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<number | null>(null);
  const textRef = useRef<HTMLHeadingElement>(null);
  const showRef = useRef<HTMLButtonElement>(null);
  const reopened = useRef(false);
  const listButtons = useRef(new Map<number, HTMLButtonElement>());

  const fetchData = async () => {
    setState({ kind: "loading" });
    try {
      const data = await load();
      setState(data === null ? { kind: "missing" } : { kind: "ready", data });
    } catch (e) {
      setState({ kind: "error", message: e instanceof Error ? e.message : String(e) });
    }
  };

  useEffect(() => {
    // Load once, the first time the section is opened.
    if (open && state.kind === "loading") void fetchData();
  }, [open]);

  useEffect(() => {
    if (selected !== null) textRef.current?.focus();
  }, [selected]);

  // After "Hide", focus goes back to "Show licences" (§7).
  useEffect(() => {
    if (!open && reopened.current) showRef.current?.focus();
  }, [open]);

  const matches = useMemo(() => {
    if (state.kind !== "ready") return [];
    const q = query.trim().toLowerCase();
    return state.data.packages
      .map((p, i) => ({ p, i }))
      .filter(({ p }) => q === "" || p.name.toLowerCase().includes(q) || p.licence.toLowerCase().includes(q));
  }, [state, query]);

  if (!open) {
    return (
      <div className="row">
        <div className="row-main">
          <span className="row-title">Open-source licences</span>
          <span className="dn-help">Almend is built on open-source software. See each package and its licence.</span>
        </div>
        <div className="row-actions">
          <Button ref={showRef} icon={<ScrollText {...ICON} aria-hidden="true" />} onClick={() => setOpen(true)}>
            Show licences
          </Button>
        </div>
      </div>
    );
  }

  return (
    <section className="licences" aria-labelledby="licences-h">
      <div className="licences-head">
        <h3 id="licences-h" className="card-subhead">
          Open-source licences
        </h3>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => {
            reopened.current = true;
            setOpen(false);
          }}
        >
          Hide
        </Button>
      </div>
      {state.kind === "loading" && (
        <div aria-busy="true" aria-label="Loading licences">
          <Skeleton width="40%" />
          <Skeleton kind="block" />
        </div>
      )}
      {state.kind === "missing" && (
        <InlineMessage tone="info">
          This build doesn't include the licence list. The released app does.
        </InlineMessage>
      )}
      {state.kind === "error" && (
        <InlineMessage
          tone="error"
          action={
            <Button size="sm" onClick={() => void fetchData()}>
              Retry
            </Button>
          }
        >
          Couldn't load the licence list. {state.message}
        </InlineMessage>
      )}
      {state.kind === "ready" && selected === null && (
        <>
          <TextField
            label="Search packages"
            type="search"
            value={query}
            placeholder="Name or licence, e.g. tokio or MIT"
            onChange={(e) => setQuery(e.target.value)}
          />
          <p className="dn-help" role="status" aria-live="polite">
            {matches.length} of {state.data.packages.length} packages
          </p>
          {matches.length === 0 ? (
            <p className="dn-help">No package matches “{query.trim()}”.</p>
          ) : (
            <ul className="licences-list" aria-label="Packages">
              {matches.map(({ p, i }) => (
                <li key={i}>
                  <button
                    type="button"
                    className="licences-item"
                    ref={(el) => {
                      if (el) listButtons.current.set(i, el);
                      else listButtons.current.delete(i);
                    }}
                    onClick={() => setSelected(i)}
                  >
                    <span className="licences-name">
                      {p.name} <span className="licences-version">{p.version}</span>
                    </span>
                    <span className="licences-licence">{p.licence}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      {state.kind === "ready" && selected !== null && (
        <LicenceText
          data={state.data}
          index={selected}
          headingRef={textRef}
          onBack={() => {
            const back = selected;
            setSelected(null);
            // Return focus to the row the user came from (§7).
            requestAnimationFrame(() => listButtons.current.get(back)?.focus());
          }}
        />
      )}
    </section>
  );
}

function LicenceText({
  data,
  index,
  headingRef,
  onBack,
}: {
  data: LicenceData;
  index: number;
  headingRef: RefObject<HTMLHeadingElement | null>;
  onBack: () => void;
}) {
  const p = data.packages[index];
  if (p === undefined) return null;
  return (
    <div className="licences-text">
      <Button size="sm" variant="ghost" icon={<ArrowLeft {...ICON} aria-hidden="true" />} onClick={onBack}>
        All packages
      </Button>
      <h4 ref={headingRef} tabIndex={-1}>
        {p.name} {p.version}
      </h4>
      <p className="dn-help">
        {SOURCE[p.source]} · {p.licence}
      </p>
      {p.texts.map((t) => (
        <pre key={t} className="licences-pre">
          {data.texts[t]}
        </pre>
      ))}
    </div>
  );
}
