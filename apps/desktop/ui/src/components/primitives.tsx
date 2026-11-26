// Design-system primitives (docs/design/design-system.md §4). Headless Radix parts +
// `--dn-*` tokens only; each component implements the states its row in §4 lists.

import * as RSelect from "@radix-ui/react-select";
import * as RSwitch from "@radix-ui/react-switch";
import * as RTooltip from "@radix-ui/react-tooltip";
import { Check, ChevronDown, Info, LoaderCircle, OctagonAlert, TriangleAlert, CircleCheck } from "lucide-react";
import {
  forwardRef,
  useId,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type KeyboardEvent,
  type ReactNode,
} from "react";

export const ICON = { size: 16, strokeWidth: 1.5 } as const;

// ---- Button: primary, secondary, ghost, danger · sm/md · default/hover/active/focus/disabled/loading

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary" | "ghost" | "danger";
  size?: "sm" | "md";
  loading?: boolean;
  icon?: ReactNode;
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", loading = false, icon, children, className, disabled, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type="button"
      {...rest}
      disabled={disabled}
      aria-busy={loading || undefined}
      className={`dn-btn dn-btn-${variant} dn-btn-${size}${loading ? " is-loading" : ""}${className ? ` ${className}` : ""}`}
    >
      {loading ? <LoaderCircle {...ICON} className="dn-spin" aria-hidden="true" /> : icon}
      {children}
    </button>
  );
});

// ---- IconButton: ghost · sm/md · + tooltip (label + shortcut) on hover/focus

type IconButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  label: string;
  shortcut?: string;
  size?: "sm" | "md";
  children: ReactNode;
};

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { label, shortcut, size = "md", children, className, ...rest },
  ref,
) {
  return (
    <RTooltip.Root>
      <RTooltip.Trigger asChild>
        <button
          ref={ref}
          type="button"
          aria-label={label}
          aria-keyshortcuts={shortcut}
          {...rest}
          className={`dn-iconbtn dn-iconbtn-${size}${className ? ` ${className}` : ""}`}
        >
          {children}
        </button>
      </RTooltip.Trigger>
      <RTooltip.Portal>
        <RTooltip.Content className="dn-tooltip" sideOffset={6}>
          {label}
          {shortcut !== undefined && <kbd>{shortcut}</kbd>}
        </RTooltip.Content>
      </RTooltip.Portal>
    </RTooltip.Root>
  );
});

export function TooltipProvider({ children }: { children: ReactNode }) {
  return <RTooltip.Provider delayDuration={400}>{children}</RTooltip.Provider>;
}

// ---- Badge: neutral, accent, local, danger, warning, success · text + icon, never colour alone

export function Badge({
  tone = "neutral",
  icon,
  children,
}: {
  tone?: "neutral" | "accent" | "local" | "danger" | "warning" | "success";
  icon?: ReactNode;
  children: ReactNode;
}) {
  return (
    <span className={`dn-badge dn-badge-${tone}`}>
      {icon}
      {children}
    </span>
  );
}

// ---- InlineMessage: info, warning, error, success

const inlineIcons = {
  info: <Info {...ICON} aria-hidden="true" />,
  warning: <TriangleAlert {...ICON} aria-hidden="true" />,
  error: <OctagonAlert {...ICON} aria-hidden="true" />,
  success: <CircleCheck {...ICON} aria-hidden="true" />,
};

export function InlineMessage({
  tone,
  children,
  action,
}: {
  tone: "info" | "warning" | "error" | "success";
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className={`dn-inline dn-inline-${tone}`} role={tone === "error" ? "alert" : "status"}>
      {inlineIcons[tone]}
      <div className="dn-inline-body">{children}</div>
      {action}
    </div>
  );
}

// ---- Skeleton: line, block

export function Skeleton({ kind = "line", width }: { kind?: "line" | "block"; width?: string }) {
  return (
    <span
      className={`dn-skeleton dn-skeleton-${kind}`}
      style={width === undefined ? undefined : { width }}
      aria-hidden="true"
    />
  );
}

// ---- EmptyState: title, one sentence, one primary action

export function EmptyState({
  title,
  children,
  action,
}: {
  title: string;
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="dn-empty">
      <h2>{title}</h2>
      <p>{children}</p>
      {action}
    </div>
  );
}

// ---- TextField: default, focus, invalid (+message), disabled, read-only

type TextFieldProps = Omit<InputHTMLAttributes<HTMLInputElement>, "id"> & {
  label: string;
  help?: ReactNode;
  error?: string | null;
};

export const TextField = forwardRef<HTMLInputElement, TextFieldProps>(function TextField(
  { label, help, error, className, ...rest },
  ref,
) {
  const id = useId();
  const describedBy = `${id}-desc`;
  const hasError = error !== undefined && error !== null && error !== "";
  return (
    <div className={`dn-field${hasError ? " is-invalid" : ""}${className ? ` ${className}` : ""}`}>
      <label htmlFor={id}>{label}</label>
      <input
        ref={ref}
        id={id}
        aria-invalid={hasError || undefined}
        aria-describedby={hasError || help !== undefined ? describedBy : undefined}
        {...rest}
      />
      {hasError ? (
        <span id={describedBy} className="dn-field-error">
          {error}
        </span>
      ) : (
        help !== undefined && (
          <span id={describedBy} className="dn-field-help">
            {help}
          </span>
        )
      )}
    </div>
  );
});

// ---- Switch: on, off, disabled · only for settings that apply immediately

export function Switch({
  label,
  description,
  checked,
  disabled,
  onCheckedChange,
}: {
  label: string;
  description?: string;
  checked: boolean;
  disabled?: boolean;
  onCheckedChange: (v: boolean) => void;
}) {
  const id = useId();
  return (
    <div className="dn-switch-row">
      <div>
        <label htmlFor={id}>{label}</label>
        {description !== undefined && (
          <p id={`${id}-d`} className="dn-help">
            {description}
          </p>
        )}
      </div>
      <RSwitch.Root
        id={id}
        className="dn-switch"
        checked={checked}
        disabled={disabled === true}
        onCheckedChange={onCheckedChange}
        aria-describedby={description === undefined ? undefined : `${id}-d`}
      >
        <RSwitch.Thumb className="dn-switch-thumb" />
      </RSwitch.Root>
    </div>
  );
}

// ---- Select: grouped options, empty results, disabled items

export interface SelectGroup {
  label: string;
  options: { value: string; label: string; disabled?: boolean; hint?: string }[];
}

/** Radix Select cannot carry an empty value; this stands for "not set". */
export const NONE = "__none__";

export function Select({
  id,
  value,
  groups,
  placeholder,
  emptyText,
  onValueChange,
  "aria-describedby": describedBy,
}: {
  id: string;
  value: string;
  groups: SelectGroup[];
  placeholder: string;
  emptyText: string;
  onValueChange: (v: string) => void;
  "aria-describedby"?: string;
}) {
  const nonEmpty = groups.filter((g) => g.options.length > 0);
  // One line: a long choice ends in "…" and its full text is the tooltip (#219).
  const shown = value === "" ? placeholder : (groups.flatMap((g) => g.options).find((o) => o.value === value)?.label ?? value);
  return (
    <RSelect.Root value={value === "" ? NONE : value} onValueChange={(v) => onValueChange(v === NONE ? "" : v)}>
      <RSelect.Trigger id={id} className="dn-select" aria-describedby={describedBy} title={shown}>
        <span className="dn-select-value">
          <RSelect.Value placeholder={placeholder} />
        </span>
        <RSelect.Icon>
          <ChevronDown {...ICON} aria-hidden="true" />
        </RSelect.Icon>
      </RSelect.Trigger>
      <RSelect.Portal>
        <RSelect.Content className="dn-select-content" position="popper" sideOffset={4}>
          <RSelect.Viewport>
            <RSelect.Item value={NONE} className="dn-select-item">
              <RSelect.ItemText>{placeholder}</RSelect.ItemText>
            </RSelect.Item>
            {nonEmpty.length === 0 && <div className="dn-select-empty">{emptyText}</div>}
            {nonEmpty.map((g) => (
              <RSelect.Group key={g.label}>
                <RSelect.Label className="dn-select-label">{g.label}</RSelect.Label>
                {g.options.map((o) => (
                  <RSelect.Item
                    key={o.value}
                    value={o.value}
                    disabled={o.disabled === true}
                    className="dn-select-item"
                  >
                    <RSelect.ItemText>{o.label}</RSelect.ItemText>
                    {o.hint !== undefined && <span className="dn-select-hint">{o.hint}</span>}
                    <RSelect.ItemIndicator className="dn-select-check">
                      <Check {...ICON} aria-hidden="true" />
                    </RSelect.ItemIndicator>
                  </RSelect.Item>
                ))}
              </RSelect.Group>
            ))}
          </RSelect.Viewport>
        </RSelect.Content>
      </RSelect.Portal>
    </RSelect.Root>
  );
}

// ---- Tabs: a tab list whose keyboard comes from its items (ui-ux tabs behaves)
//
// Left/Right move to the previous/next tab and select it (automatic activation), Home/End
// go to the first/last; only the selected tab is in the Tab order. The caller renders the
// panel with `tabPanelProps(idBase, value)`.

export interface TabItem<T extends string> {
  id: T;
  label: string;
}

export function Tabs<T extends string>({
  label,
  idBase,
  items,
  value,
  onChange,
}: {
  /** Accessible name of the tab list. */
  label: string;
  idBase: string;
  items: TabItem<T>[];
  value: T;
  onChange: (id: T) => void;
}) {
  const move = (e: KeyboardEvent<HTMLDivElement>) => {
    const i = items.findIndex((t) => t.id === value);
    const last = items.length - 1;
    const to =
      e.key === "ArrowRight" ? (i + 1) % items.length
      : e.key === "ArrowLeft" ? (i - 1 + items.length) % items.length
      : e.key === "Home" ? 0
      : e.key === "End" ? last
      : null;
    if (to === null || items[to] === undefined) return;
    e.preventDefault();
    const next = items[to].id;
    onChange(next);
    e.currentTarget.querySelector<HTMLButtonElement>(`#${idBase}-tab-${next}`)?.focus();
  };
  return (
    <div className="tabs" role="tablist" aria-label={label} onKeyDown={move}>
      {items.map((t) => (
        <button
          key={t.id}
          type="button"
          role="tab"
          id={`${idBase}-tab-${t.id}`}
          aria-controls={`${idBase}-panel-${t.id}`}
          aria-selected={t.id === value}
          tabIndex={t.id === value ? 0 : -1}
          className={t.id === value ? "tab selected" : "tab"}
          onClick={() => onChange(t.id)}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

export function tabPanelProps(idBase: string, value: string) {
  return {
    role: "tabpanel" as const,
    id: `${idBase}-panel-${value}`,
    "aria-labelledby": `${idBase}-tab-${value}`,
    className: "tabpanel",
  };
}
