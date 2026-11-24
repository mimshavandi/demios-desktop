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

/** Running on Windows (WebView2). */
export const isWindows = typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent);

/**
 * Why artifacts can't be shown in this build, or `null` when they can. On Windows, WebView2 runs
 * the app's init scripts (Tauri's IPC object) in sub-frames too, and they can't be removed from
 * the artifact frame, so artifacts stay off there for now (threat model R8).
 */
export const ARTIFACTS_UNAVAILABLE: string | null = isWindows
  ? "Artifacts aren't available on Windows yet. They are saved, and will open here in a later version."
  : null;
