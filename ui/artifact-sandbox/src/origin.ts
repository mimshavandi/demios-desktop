/**
 * Whether `frameUrl` is on a different origin than the host page. Compares scheme + host + port
 * rather than `URL.origin`, which is `"null"` for custom schemes such as `artifact://` and
 * `tauri://` and would make any two of them look alike (or a same-scheme URL look separate).
 */
export function isSeparateOrigin(frameUrl: string, hostHref: string): boolean {
  const f = new URL(frameUrl, hostHref);
  const h = new URL(hostHref);
  return f.protocol !== h.protocol || f.host !== h.host;
}
