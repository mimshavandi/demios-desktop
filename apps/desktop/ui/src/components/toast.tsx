// Toasts (design-system §4): info/success/warning/error, role=status (errors alert),
// auto-dismiss after 5 s except errors, optional Undo, at most 3 stacked.

import * as RToast from "@radix-ui/react-toast";
import { X } from "lucide-react";
import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { ICON } from "./primitives";

export interface ToastSpec {
  tone: "info" | "success" | "warning" | "error";
  title: string;
  /** Visible duration in ms; default 5000, errors stay until dismissed. */
  duration?: number;
  action?: { label: string; onAction: () => void };
  /** Called when the toast goes away without its action being used. */
  onExpire?: () => void;
}

interface Live extends ToastSpec {
  id: number;
  acted: boolean;
}

const Ctx = createContext<(t: ToastSpec) => void>(() => {});

export function useToast() {
  return useContext(Ctx);
}

const MAX = 3;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Live[]>([]);
  // Mirror of `toasts` so callbacks run outside state updaters (StrictMode runs
  // updaters twice; an onExpire in there would fire twice).
  const live = useRef<Live[]>([]);
  const nextId = useRef(1);

  const commit = useCallback((next: Live[]) => {
    live.current = next;
    setToasts(next);
  }, []);

  const close = useCallback(
    (id: number) => {
      const t = live.current.find((x) => x.id === id);
      if (t === undefined) return;
      commit(live.current.filter((x) => x.id !== id));
      if (!t.acted) t.onExpire?.();
    },
    [commit],
  );

  const push = useCallback(
    (spec: ToastSpec) => {
      const next = [...live.current, { ...spec, id: nextId.current++, acted: false }];
      // Overflowing toasts expire now (their pending action, if any, goes through).
      const drop = next.slice(0, Math.max(0, next.length - MAX));
      commit(next.slice(-MAX));
      for (const t of drop) if (!t.acted) t.onExpire?.();
    },
    [commit],
  );

  const value = useMemo(() => push, [push]);

  return (
    <Ctx.Provider value={value}>
      <RToast.Provider swipeDirection="right">
        {children}
        {toasts.map((t) => (
          <RToast.Root
            key={t.id}
            className={`dn-toast dn-toast-${t.tone}`}
            type={t.tone === "error" ? "foreground" : "background"}
            duration={t.duration ?? (t.tone === "error" ? Infinity : 5000)}
            onOpenChange={(open) => {
              if (!open) close(t.id);
            }}
          >
            <RToast.Title className="dn-toast-title">{t.title}</RToast.Title>
            {t.action !== undefined && (
              <RToast.Action asChild altText={t.action.label}>
                <button
                  type="button"
                  className="dn-btn dn-btn-secondary dn-btn-sm"
                  onClick={() => {
                    t.acted = true;
                    t.action?.onAction();
                  }}
                >
                  {t.action.label}
                </button>
              </RToast.Action>
            )}
            <RToast.Close className="dn-iconbtn dn-iconbtn-sm" aria-label="Dismiss">
              <X {...ICON} aria-hidden="true" />
            </RToast.Close>
          </RToast.Root>
        ))}
        <RToast.Viewport className="dn-toast-viewport" />
      </RToast.Provider>
    </Ctx.Provider>
  );
}
