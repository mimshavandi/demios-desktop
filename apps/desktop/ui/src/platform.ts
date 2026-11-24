export const isMac =
  typeof navigator !== "undefined" && /Mac/i.test(navigator.platform || navigator.userAgent);

/** Modifier shown in shortcut hints: ⌘ on macOS, Ctrl elsewhere (design-system §7). */
export const mod = isMac ? "⌘" : "Ctrl+";

/**
 * Frame shell of the artifact origin (T-ART-06): the app's `artifact` URI scheme, which Tauri
 * serves as `artifact://localhost` on macOS/Linux and `http://artifact.localhost` on Windows.
 * In a plain browser (`npm run dev` with the IPC mock) the sandbox's own dev server stands in
 * (`npm run serve` in ui/artifact-sandbox).
 */
export function artifactFrameUrl(): string {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    return "http://localhost:4175/frame/";
  }
  return /Windows/i.test(navigator.userAgent) ? "http://artifact.localhost/frame/" : "artifact://localhost/frame/";
}
