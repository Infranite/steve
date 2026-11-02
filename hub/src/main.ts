/* Steve Hub entry: one HTTP server — /mcp for agent tools, /auth and /api for the web app, static files for the rest. */
import { createServer } from "node:http"
import { readFileSync } from "node:fs"
import { join, relative } from "node:path"
import { Hono, type Context } from "hono"
import { getRequestListener } from "@hono/node-server"
import { serveStatic } from "@hono/node-server/serve-static"
import { config, webVersion } from "./config.js"
import { ensureWorkspace } from "./repo.js"
import { authRoutes, requireUser } from "./auth.js"
import { api } from "./api.js"
import { handleMcp } from "./mcp.js"
import { seed, recover, watchReleases, drain } from "./orchestrator.js"
import { registerHub } from "./system.js"
import { housekeeping } from "./housekeeping.js"
import * as acp from "./acp.js"

ensureWorkspace(); seed(); registerHub(); recover(); watchReleases()
housekeeping(); setInterval(housekeeping, 3600_000).unref()

const app = new Hono()
/* `version` is the Hub (ops/release.sh waits for it after a restart); the web build is released on its own, without a restart */
app.get("/healthz", (c) => c.json({ ok: true, version: config.version, web_version: webVersion() }))
app.route("/auth", authRoutes)
/* the release script's hand-off before it restarts the Hub: loopback only, with the token the Hub gave that release */
const drainCall = (start: boolean) => (c: Context) => {
  const ip = (c.env as any)?.incoming?.socket?.remoteAddress ?? "", auth = c.req.header("authorization") ?? ""
  const r = /^(127\.|::1|::ffff:127\.)/.test(ip) && auth.startsWith("Bearer ") ? drain(auth.slice(7), start) : null
  return r ? c.json(r) : c.text("not found", 404)
}
app.post("/internal/release/drain", drainCall(true))
app.get("/internal/release/drain", drainCall(false))
app.use("/api/*", requireUser)
app.route("/api", api)
/* the web build: config.webDir is web/current, a symlink that ops/release-web.sh switches while the Hub runs, so every
 * request resolves it again (serveStatic stats per request; nothing is cached here). index.html names the hashed assets of
 * its build, so it is never cached: a reload after a web release always gets the new one; the hashed assets are immutable. */
const webRoot = relative(process.cwd(), config.webDir) || "."
app.get("/release.json", (c) => c.text("not found", 404))
app.use("/*", async (c, next) => {
  await next()
  if (c.res.status !== 200 || c.res.headers.has("cache-control")) return
  const p = c.req.path
  if (p === "/" || p.endsWith(".html")) c.res.headers.set("cache-control", "no-store")
  else if (p.startsWith("/assets/")) c.res.headers.set("cache-control", "public, max-age=31536000, immutable")
})
app.use("/*", serveStatic({ root: webRoot }))
/* the SPA fallback; before the first web release there is no index.html yet. A missing hashed asset (a tab still on a
 * pruned build) is a 404, not index.html: the browser would fail on HTML as a script, and the release check relies on it */
app.get("*", (c) => {
  if (c.req.path.startsWith("/assets/")) return c.text("not found", 404)
  let html: string
  try { html = readFileSync(join(config.webDir, "index.html"), "utf8") } catch { return c.text("Steve web is not installed", 503, { "cache-control": "no-store" }) }
  return c.html(html, 200, { "cache-control": "no-store" })
})

const honoListener = getRequestListener(app.fetch)
const server = createServer((req, res) => {
  if (req.url?.startsWith("/mcp")) {
    /* only local agents may reach Steve's tools */
    const ip = req.socket.remoteAddress ?? ""
    if (!/^(127\.|::1|::ffff:127\.)/.test(ip)) { res.writeHead(403).end(); return }
    void handleMcp(req, res); return
  }
  void honoListener(req, res)
})
server.listen(config.listen.port, config.listen.host, () => console.log(`steve hub ${config.version} on http://${config.listen.host}:${config.listen.port}`))
const stop = () => { acp.shutdown(); server.close(); process.exit(0) }
process.on("SIGINT", stop); process.on("SIGTERM", stop)
