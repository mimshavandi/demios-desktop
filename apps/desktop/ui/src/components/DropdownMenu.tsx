import * as RMenu from "@radix-ui/react-dropdown-menu";
import { Check, ChevronRight } from "lucide-react";
import { useRef, type ReactNode } from "react";
import { errorMessage } from "../ipc/types";
import { ICON, IconButton } from "./primitives";
import { useToast } from "./toast";

/**
 * One entry of a {@link DropdownMenu}.
 * - `item`: an action. `run` may be async; if it fails, the menu shows `failure` with the
 *   error as a toast. Nothing is retried or swallowed.
 * - `choice`: a submenu where exactly one value is chosen (the current one is marked).
 * - `separator`.
 */
export type MenuEntry =
  | {
      kind: "item";
      label: string;
      icon?: ReactNode;
      danger?: boolean;
      /** Why it can't be used right now; the item shows disabled with this as its hint. */
      disabled?: string;
      /** Toast title when `run` fails, e.g. "Couldn't delete the chat". */
      failure: string;
      run: () => void | Promise<void>;
      /**
       * Where focus goes once the menu has closed, when not back to the trigger (it may be
       * gone, or the action opened a field). Called after `run` starts.
       */
      focusAfter?: () => HTMLElement | null;
    }
  | {
      kind: "choice";
      label: string;
      icon?: ReactNode;
      /** Accessible name of the choice group inside the submenu. */
      groupLabel: string;
      value: string;
      options: { value: string; label: string }[];
      failure: string;
      onChoose: (value: string) => void | Promise<void>;
    }
  | { kind: "separator" };

/**
 * Design-system menu (ADR-0010 §4) on Radix DropdownMenu: an icon trigger with a name;
 * arrow keys, Home/End, typeahead and Esc; submenus; focus back to the trigger on close
 * (or to `focusAfter`). Failures of an action show as a toast (#166, no silent fallbacks).
 */
export function DropdownMenu({
  label,
  trigger,
  entries,
  triggerClassName,
  testId,
}: {
  /** The trigger's accessible name, e.g. "More actions for “Report”". */
  label: string;
  trigger: ReactNode;
  entries: MenuEntry[];
  triggerClassName?: string;
  testId?: string;
}) {
  const toast = useToast();
  const focusNext = useRef<(() => HTMLElement | null) | null>(null);

  const attempt = (failure: string, f: () => void | Promise<void>) => {
    try {
      const r = f();
      if (r instanceof Promise) {
        r.catch((e: unknown) => toast({ tone: "error", title: `${failure}. ${errorMessage(e)}` }));
      }
    } catch (e) {
      toast({ tone: "error", title: `${failure}. ${errorMessage(e)}` });
    }
  };

  return (
    <RMenu.Root modal={false}>
      <RMenu.Trigger asChild>
        <IconButton size="sm" label={label} className={triggerClassName} data-testid={testId}>
          {trigger}
        </IconButton>
      </RMenu.Trigger>
      <RMenu.Portal>
        <RMenu.Content
          className="dn-menu"
          align="end"
          sideOffset={4}
          onCloseAutoFocus={(e) => {
            const next = focusNext.current;
            focusNext.current = null;
            if (next === null) return; // Radix puts focus back on the trigger.
            e.preventDefault();
            // After React has rendered what the action changed (a field, a removed row).
            requestAnimationFrame(() => next()?.focus());
          }}
        >
          {entries.map((entry, i) => {
            if (entry.kind === "separator") return <RMenu.Separator key={`sep-${i}`} className="dn-menu-sep" />;
            if (entry.kind === "item") {
              return (
                <RMenu.Item
                  key={entry.label}
                  className={`dn-menu-item${entry.danger === true ? " is-danger" : ""}`}
                  disabled={entry.disabled !== undefined}
                  onSelect={() => {
                    focusNext.current = entry.focusAfter ?? null;
                    attempt(entry.failure, entry.run);
                  }}
                >
                  {entry.icon}
                  <span>{entry.label}</span>
                  {entry.disabled !== undefined && <span className="dn-menu-hint">{entry.disabled}</span>}
                </RMenu.Item>
              );
            }
            return (
              <RMenu.Sub key={entry.label}>
                <RMenu.SubTrigger className="dn-menu-item">
                  {entry.icon}
                  <span>{entry.label}</span>
                  <ChevronRight {...ICON} className="dn-menu-chevron" aria-hidden="true" />
                </RMenu.SubTrigger>
                <RMenu.Portal>
                  <RMenu.SubContent className="dn-menu" sideOffset={4} alignOffset={-4}>
                    <RMenu.RadioGroup
                      aria-label={entry.groupLabel}
                      value={entry.value}
                      onValueChange={(v) => {
                        if (v !== entry.value) attempt(entry.failure, () => entry.onChoose(v));
                      }}
                    >
                      {entry.options.map((o) => (
                        <RMenu.RadioItem key={o.value} value={o.value} className="dn-menu-item">
                          <span>{o.label}</span>
                          <RMenu.ItemIndicator className="dn-menu-check">
                            <Check {...ICON} aria-hidden="true" />
                          </RMenu.ItemIndicator>
                        </RMenu.RadioItem>
                      ))}
                    </RMenu.RadioGroup>
                  </RMenu.SubContent>
                </RMenu.Portal>
              </RMenu.Sub>
            );
          })}
        </RMenu.Content>
      </RMenu.Portal>
    </RMenu.Root>
  );
}
