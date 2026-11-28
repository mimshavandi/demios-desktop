import { useId } from "react";

/** A mask id that's safe inside `url(#…)` (React's ids contain colons). */
function useMaskId(): string {
  return `almend-meadow-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
}

/**
 * The Almend logo (design: Logo, chosen by the owner 2026-11-28): the "Alm" mark, two
 * summits with a meadow line, and the lowercase wordmark. Drawn in `currentColor`, so the
 * colour comes from the surrounding text: ink on light grounds, white on dark. The meadow
 * line is a gap through to the background, never a second colour. Sources and rules: the
 * Design System's Logos group and docs/design/design-system.md (Logo).
 */

/** The mark's outline, in its own 120 × 120 box. */
const MARK =
  "M 6 100 C 20 98 34 46 48 34 L 56 40 C 62 30 66 24 72 24 C 82 26 92 58 102 74 C 108 84 111 92 114 100 Z";
/** The meadow line, cut out of the mark. */
const MEADOW = "M 2 86 Q 50 70 118 88";

/** Mask that keeps the mark and cuts the meadow line out (keyword colours: a mask, not a colour). */
function MeadowMask({ id, width }: { id: string; width: number }) {
  return (
    <mask id={id} maskUnits="userSpaceOnUse" x="0" y="0" width="120" height="120">
      <rect width="120" height="120" fill="white" />
      <path d={MEADOW} stroke="black" strokeWidth={width} fill="none" />
    </mask>
  );
}

/**
 * The mark alone, at `size` (24 px or more: below that the design uses the favicon file).
 * `label` names it for screen readers; leave it out where it's decoration, and it's hidden.
 */
export function AlmendMark({ size, label, className }: { size: string; label?: string; className?: string }) {
  const id = useMaskId();
  return (
    <svg
      viewBox="0 0 120 120"
      style={{ width: size, height: size }}
      className={className}
      role={label === undefined ? undefined : "img"}
      aria-label={label}
      aria-hidden={label === undefined ? true : undefined}
      focusable="false"
    >
      <MeadowMask id={id} width={7} />
      <path d={MARK} fill="currentColor" mask={`url(#${id})`} />
    </svg>
  );
}

/** The wordmark's strokes, in the lockup's box. Artwork, never re-set in a font. */
function Wordmark() {
  return (
    <g fill="none" stroke="currentColor" strokeWidth="9" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="20" cy="40" r="15.5" />
      <path d="M 35.5 22 V 60" />
      <path d="M 52 2 V 60" />
      <path d="M 64 22 V 60 M 64 36 A 12 12 0 0 1 88 36 V 60 M 88 36 A 12 12 0 0 1 112 36 V 60" />
      <path d="M 126.5 40 H 157.5 A 15.5 15.5 0 1 0 153.9 50" />
      <path d="M 172 22 V 60 M 172 36 A 12 12 0 0 1 196 36 V 60" />
      <circle cx="224" cy="40" r="15.5" />
      <path d="M 239.5 2 V 60" />
    </g>
  );
}

/** The horizontal lockup (mark + wordmark), for headers. `height` at least 24 px (design). */
export function AlmendLockup({ height, label = "Almend", className }: { height: string; label?: string; className?: string }) {
  const id = useMaskId();
  return (
    <svg
      viewBox="0 0 370 84"
      style={{ height, width: "auto" }}
      className={className}
      role="img"
      aria-label={label}
      focusable="false"
    >
      <MeadowMask id={id} width={7} />
      <g transform="translate(0 -8.5) scale(0.85)">
        <path d={MARK} fill="currentColor" mask={`url(#${id})`} />
      </g>
      <g transform="translate(116 12)">
        <Wordmark />
      </g>
    </svg>
  );
}
