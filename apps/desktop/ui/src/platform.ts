export const isMac =
  typeof navigator !== "undefined" && /Mac/i.test(navigator.platform || navigator.userAgent);

/** Modifier shown in shortcut hints: ⌘ on macOS, Ctrl elsewhere (design-system §7). */
export const mod = isMac ? "⌘" : "Ctrl+";
