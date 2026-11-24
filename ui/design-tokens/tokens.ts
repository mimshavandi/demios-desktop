// GENERATED from tokens.json by scripts/build.mjs — do not edit. ADR-0010.
export type ColorToken = "bg" | "surface" | "surface-sunken" | "surface-raised" | "sidebar" | "border" | "border-strong" | "text" | "text-muted" | "text-subtle" | "text-on-accent" | "accent" | "accent-hover" | "accent-soft" | "accent-text" | "local" | "local-soft" | "danger" | "danger-soft" | "warning" | "warning-soft" | "success" | "success-soft" | "focus" | "code-bg" | "overlay" | "text-on-danger";
export type SpaceToken = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "8" | "10" | "12" | "16";
export type RadiusToken = "sm" | "md" | "lg" | "xl" | "full";
export type TextToken = "xs" | "sm" | "base" | "md" | "lg" | "xl" | "2xl";
/** `var(--dn-color-…)` for a colour role. Never use raw hex in components. */
export const color = (role: ColorToken): string => `var(--dn-color-${role})`;
export const space = (step: SpaceToken): string => `var(--dn-space-${step})`;
export const radius = (r: RadiusToken): string => `var(--dn-radius-${r})`;
