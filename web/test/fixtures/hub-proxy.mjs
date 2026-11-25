/* Lightweight loopback HTTP reverse proxy for browser tests with an in-process mock Hub.
 * Routes `/api/*` and `/auth/*` to `hubUrl` and forwards web/Vite requests to the shared
 * `STEVE_TEST_WEB` Vite dev server, so concurrent tests never compete over `node_modules/.vite`.
 * Falls back to a private Vite server with an isolated temp cacheDir when run standalone. */
import { createServer as createHttp, request as httpRequest } from "node:http"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

// Chromium flags that refuse off-loopback DNS lookups without registering a catch-all page.route,
// which would otherwise disable Chromium's HTTP cache for static Vite modules.
export const OFFLINE_CHROMIUM_ARGS = ["--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1"]

/** Vite's dev server sends `Cache-Control: no-cache` for source files, which never change during a single
 *  test run; caching them for one hour lets repeated page loads in the same context reuse Chromium's cache. */
const STATIC_ASSET_CACHE_MAX_AGE_SECS = 3600

export async function createWebProxy({ hubUrl = null, onRequest = null, responseHeaders = null, cacheAssets = false } = {}) {
  let ownVite = null, tempCache = null
  let viteBase = process.env.STEVE_TEST_WEB || ""
  if (!viteBase) {
    const { createServer } = await import("vite")
    const root = fileURLToPath(new URL("../..", import.meta.url))
    tempCache = mkdtempSync(join(tmpdir(), "steve-vite-"))
    ownVite = await createServer({
      root, configFile: join(root, "vite.config.ts"), cacheDir: tempCache, logLevel: "silent",
      server: { host: "127.0.0.1", port: 0, strictPort: true, hmr: false, forwardConsole: false },
    })
    await ownVite.listen()
    viteBase = `http://127.0.0.1:${ownVite.httpServer.address().port}`
  }
  const viteTarget = new URL(viteBase)
  const hubTarget = hubUrl ? new URL(hubUrl) : null
  const server = createHttp((req, res) => {
    if (onRequest && onRequest(req, res) === true) return
    const path = (req.url || "/").split("?")[0]
    const toHub = hubTarget && (path.startsWith("/api/") || path.startsWith("/auth/"))
    const target = toHub ? hubTarget : viteTarget
    const upstream = httpRequest({
      hostname: target.hostname,
      port: target.port,
      path: req.url,
      method: req.method,
      headers: { ...req.headers, host: toHub ? req.headers.host : viteTarget.host },
    }, (up) => {
      const headers = { ...up.headers }
      if (cacheAssets && !toHub && req.method === "GET" && up.statusCode === 200 && !path.startsWith("/api/") && !path.startsWith("/auth/")) {
        headers["cache-control"] = `private, max-age=${STATIC_ASSET_CACHE_MAX_AGE_SECS}`
      }
      if (responseHeaders) Object.assign(headers, responseHeaders(req, up) || {})
      res.writeHead(up.statusCode || 502, headers)
      up.pipe(res)
    })
    upstream.on("error", () => { if (!res.headersSent) res.writeHead(502); res.end() })
    req.pipe(upstream)
  })
  await new Promise((r) => server.listen(0, "127.0.0.1", r))
  const url = `http://127.0.0.1:${server.address().port}`
  return {
    url,
    close: async () => {
      server.closeAllConnections()
      await new Promise((r) => server.close(r))
      if (ownVite) await ownVite.close()
      if (tempCache) rmSync(tempCache, { recursive: true, force: true })
    },
  }
}
