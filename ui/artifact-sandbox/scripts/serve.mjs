// Static server for the demo/test page plus a second-origin "attacker" server that records every
// request it receives (the escape tests assert it stays empty). Test-only; binds 127.0.0.1.
import { createServer } from "node:http";
import { createSocket } from "node:dgram";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(fileURLToPath(new URL("..", import.meta.url)), "dist");
const APP_PORT = Number(process.env.APP_PORT ?? 4173);
const SPY_PORT = Number(process.env.SPY_PORT ?? 4174);
const ART_PORT = Number(process.env.ART_PORT ?? 4175);
// The artifact origin uses a different host name so test cookies (which ignore ports) differ.
const ART_ORIGIN = `http://localhost:${ART_PORT}`;
const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css" };

// Same policy the desktop shell must set for its main webview (docs/security/artifact-sandbox.md).
const HOST_CSP = [
  "default-src 'self'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self' data:",
  "connect-src 'self'",
  `frame-src ${ART_ORIGIN}`,
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
].join("; ");

const appHits = [];
createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", "http://x");
  appHits.push(url.pathname + url.search);
  if (url.pathname === "/__hits") {
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(appHits));
    return;
  }
  const rel = url.pathname === "/" ? "/demo/index.html" : url.pathname;
  const file = normalize(join(root, rel));
  if (!file.startsWith(root)) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res.writeHead(200, {
      "content-type": TYPES[extname(file)] ?? "application/octet-stream",
      "content-security-policy": HOST_CSP,
      "set-cookie": "dn_session=app-secret; Path=/; SameSite=Strict",
      "cache-control": "no-store",
    });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
}).listen(APP_PORT, "127.0.0.1");

const spyHits = [];
const spy = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://x");
  if (url.pathname === "/__log") {
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(spyHits));
    return;
  }
  if (url.pathname === "/__reset") {
    spyHits.length = 0;
    return res.writeHead(204).end();
  }
  spyHits.push(`${req.method} ${url.pathname}${url.search}`);
  res.writeHead(200, { "access-control-allow-origin": "*", "content-type": "text/plain" }).end("leaked");
});
spy.on("upgrade", (req, socket) => {
  spyHits.push(`UPGRADE ${req.url}`);
  socket.destroy();
});
spy.listen(SPY_PORT, "127.0.0.1");
// UDP on the same port number catches WebRTC STUN/TURN traffic.
const udp = createSocket("udp4");
udp.on("message", (_msg, rinfo) => spyHits.push(`UDP from ${rinfo.address}:${rinfo.port}`));
udp.bind(SPY_PORT, "127.0.0.1");

// Artifact origin: serves only the frame shell, with the artifact CSP as a header.
const csp = JSON.parse(await readFile(join(root, "frame", "csp.json"), "utf8"));
const shell = await readFile(join(root, "frame", "index.html"));
const artHits = [];
const artifactHandler = (req, res) => {
  const url = new URL(req.url ?? "/", "http://x");
  artHits.push(url.pathname + url.search);
  if (url.pathname === "/__log") {
    res.writeHead(200, { "content-type": "application/json", "access-control-allow-origin": "*" }).end(JSON.stringify(artHits));
    return;
  }
  if (url.pathname !== "/frame/" && url.pathname !== "/frame/index.html") return res.writeHead(404).end();
  res.writeHead(200, {
    "content-type": "text/html; charset=utf-8",
    "content-security-policy": url.searchParams.get("net") === "1" ? csp.network : csp.strict,
    "x-content-type-options": "nosniff",
    "cache-control": "no-store",
  });
  res.end(shell);
};
// `localhost` may resolve to either loopback address.
createServer(artifactHandler).listen(ART_PORT, "127.0.0.1");
createServer(artifactHandler).listen(ART_PORT, "::1");

console.log(`app http://127.0.0.1:${APP_PORT}  spy http://127.0.0.1:${SPY_PORT}  artifacts ${ART_ORIGIN}`);
