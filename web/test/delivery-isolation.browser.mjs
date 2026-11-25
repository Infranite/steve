/* Isolated HTTP Hub fixture + Vite: interactive HTML previews are isolated (#205).
 * Exercises Chromium's real sandbox="allow-scripts", Hub frame CSP, delivery page frame-src CSP,
 * Sec-Fetch-Dest: document denial, share toggle-off invalidation, markdown package images,
 * WAV range 206 playback, and #/publications without needing an external Hub binary. */
import assert from "node:assert/strict"
import { randomBytes, randomUUID } from "node:crypto"
import { mkdir, writeFile } from "node:fs/promises"
import { createServer as createHttp } from "node:http"
import { crc32 } from "node:zlib"
import { createWebProxy } from "./fixtures/hub-proxy.mjs"

const output = process.env.STEVE_TEST_SHOTS
if (output) await mkdir(output, { recursive: true })

/* "the internet": any hit is a leak */
const leaks = []
const outside = createHttp((req, res) => { leaks.push(`${req.method} ${req.url}`); res.writeHead(200, { "access-control-allow-origin": "*", "content-type": "image/png" }); res.end() })
await new Promise((r) => outside.listen(0, "127.0.0.1", r))
const away = `http://127.0.0.1:${outside.address().port}`

function zip(files) {
  const data = [], central = []; let offset = 0
  for (const [name, body] of Object.entries(files)) {
    const b = Buffer.from(body), n = Buffer.from(name), crc = crc32(b)
    const local = Buffer.alloc(30); local.writeUInt32LE(0x04034b50); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x0800, 6)
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(b.length, 18); local.writeUInt32LE(b.length, 22); local.writeUInt16LE(n.length, 26)
    const entry = Buffer.alloc(46); entry.writeUInt32LE(0x02014b50); entry.writeUInt16LE(20, 4); entry.writeUInt16LE(20, 6); entry.writeUInt16LE(0x0800, 8)
    entry.writeUInt32LE(crc, 16); entry.writeUInt32LE(b.length, 20); entry.writeUInt32LE(b.length, 24); entry.writeUInt16LE(n.length, 28); entry.writeUInt32LE(offset, 42)
    data.push(local, n, b); central.push(entry, n); offset += local.length + n.length + b.length
  }
  const directory = Buffer.concat(central), end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50); end.writeUInt16LE(central.length / 2, 8); end.writeUInt16LE(central.length / 2, 10); end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16)
  return Buffer.concat([...data, directory, end])
}
function unzipStore(buf) {
  const out = {}; let i = 0
  while (i + 30 <= buf.length && buf.readUInt32LE(i) === 0x04034b50) {
    const nLen = buf.readUInt16LE(i + 26), eLen = buf.readUInt16LE(i + 28), size = buf.readUInt32LE(i + 18)
    const name = buf.subarray(i + 30, i + 30 + nLen).toString("utf8")
    const start = i + 30 + nLen + eLen
    out[name] = buf.subarray(start, start + size)
    i = start + size
  }
  return out
}
const mimeOf = (name) => name.endsWith(".html") ? "text/html; charset=utf-8" : name.endsWith(".js") ? "text/javascript; charset=utf-8"
  : name.endsWith(".css") ? "text/css; charset=utf-8" : name.endsWith(".png") ? "image/png" : name.endsWith(".md") ? "text/markdown; charset=utf-8"
  : name.endsWith(".wav") ? "audio/wav" : "application/octet-stream"
const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aWQAAAABJRU5ErkJggg==", "base64")
/* 0.2 s of silence, 8 kHz mono 8-bit PCM: a real file the browser decodes */
function wav() {
  const n = 1600, b = Buffer.alloc(44 + n, 128)
  b.write("RIFF", 0); b.writeUInt32LE(36 + n, 4); b.write("WAVEfmt ", 8); b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22)
  b.writeUInt32LE(8000, 24); b.writeUInt32LE(8000, 28); b.writeUInt16LE(1, 32); b.writeUInt16LE(8, 34); b.write("data", 36); b.writeUInt32LE(n, 40)
  return b
}

const staticHtmlCsp = "sandbox allow-same-origin; default-src 'none'; script-src 'none'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; media-src 'self'; connect-src 'none'; object-src 'none'; frame-src 'none'; worker-src 'none'; base-uri 'none'; form-action 'none'"
const frameCsp = (prefix, base) => `sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' ${prefix}; style-src 'unsafe-inline' ${prefix}; img-src ${prefix} data: blob:; font-src ${prefix} data:; media-src ${prefix} data: blob:; connect-src 'none'; form-action 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; object-src 'none'; manifest-src 'none'; base-uri 'none'; frame-ancestors 'self' ${base}`

let base = ""
const deliveries = [], byId = new Map(), grants = new Map(), shares = new Map()
let seq = 0
const wsDefault = { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }
const deliveryMeta = (d, token = null) => {
  const rootPath = token ? `/api/delivery-shares/${encodeURIComponent(token)}` : `/api/file-deliveries/${encodeURIComponent(d.id)}`
  const frameUrl = d.interactive ? (token ? `${rootPath}/frame/${d.entrypoint}` : `/api/delivery-frames/${d.grant}/${d.entrypoint}`) : null
  return {
    id: d.id, item_id: null, title: d.title, name: d.name, mime: d.mime, size: d.body.length, created: d.created,
    entrypoint: d.entrypoint,
    url: base + (token ? `/shares/${token}` : `/deliveries/${d.id}?ws=default`),
    download_url: rootPath + "/raw" + (token ? "" : "?ws=default"),
    content_url: rootPath + (d.entrypoint ? "/content/" : "/content/file") + (token ? "" : "?ws=default"),
    access_warning: null, interactive: d.interactive, scripts: d.scripts, frame_url: frameUrl, can_manage: !token,
    ...(!token ? { workspace_id: "default" } : { expires_at: shares.get(token).expires_at }),
  }
}

const hub = createHttp(async (req, res) => {
  const u = new URL(req.url, "http://127.0.0.1"), p = u.pathname
  const json = (v, status = 200) => { res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" }); res.end(JSON.stringify(v)) }
  const authed = (req.headers.cookie || "").includes("steve_session_fixture=1")

  if (p === "/auth/hello") return json({ name: "steve", version: "fixture", api: 2, login: { token: false, oidc: false } })
  if (p === "/auth/dev") { res.writeHead(200, { "content-type": "application/json", "set-cookie": "steve_session_fixture=1; HttpOnly; SameSite=Lax; Path=/" }); return res.end('{"ok":true}') }

  // Frame capability route (opaque origin; never requires cookies; refuses top-level document navigations).
  const frameMatch = /^\/api\/delivery-frames\/([0-9a-f]{64})\/(.+)$/.exec(p)
  if (frameMatch && req.method === "GET") {
    if (req.headers["sec-fetch-dest"] === "document") return json({ error: "frame only" }, 403)
    const d = grants.get(frameMatch[1]), rel = frameMatch[2]
    if (!d || !d.interactive || !d.files?.[rel]) return json({ error: "not_found" }, 404)
    const prefix = `${base}/api/delivery-frames/${frameMatch[1]}/`
    res.writeHead(200, { "content-type": mimeOf(rel), "cache-control": "no-store", "x-content-type-options": "nosniff", "content-security-policy": frameCsp(prefix, base) })
    return res.end(d.files[rel])
  }

  // Public share routes (token-scoped, no cookies required).
  const shareMatch = /^\/api\/delivery-shares\/([^/]+)(.*)$/.exec(p)
  if (shareMatch) {
    const token = decodeURIComponent(shareMatch[1]), rest = shareMatch[2], s = shares.get(token)
    if (!s || s.revoked_at || s.expires_at <= Date.now()) return json({ error: "gone" }, 410)
    const d = byId.get(s.id)
    if (!rest) return json(deliveryMeta(d, token))
    if (rest.startsWith("/frame/")) {
      if (req.headers["sec-fetch-dest"] === "document") return json({ error: "frame only" }, 403)
      const rel = rest.slice(7)
      if (!d.interactive || !d.files?.[rel]) return json({ error: "not_found" }, 404)
      const prefix = `${base}/api/delivery-shares/${encodeURIComponent(token)}/frame/`
      res.writeHead(200, { "content-type": mimeOf(rel), "cache-control": "no-store", "x-content-type-options": "nosniff", "content-security-policy": frameCsp(prefix, base) })
      return res.end(d.files[rel])
    }
    if (rest.startsWith("/content/")) {
      const rel = rest.slice(9), body = d.files ? d.files[rel] : d.body, mime = d.files ? mimeOf(rel) : d.mime
      if (!body) return json({ error: "not_found" }, 404)
      res.writeHead(200, { "content-type": mime, "cache-control": "no-store", ...(mime.startsWith("text/html") ? { "content-security-policy": staticHtmlCsp } : {}) })
      return res.end(body)
    }
    if (rest === "/raw") { res.writeHead(200, { "content-type": d.mime }); return res.end(d.body) }
    return json({ error: "not_found" }, 404)
  }

  if (!authed) return json({ error: "unauthenticated" }, 401)
  if (p === "/api/shell") return json({ version: "fixture", web_version: "fixture", owner: true, workspace: wsDefault, workspaces: [wsDefault], bots: [], allBots: [], projects: [], tags: [], settings: { onboard: "1" } })
  if (p === "/api/summary") return json({ ws: "default", thinking: false, hold: null })
  if (p === "/api/snapshot") return json({ epoch: "fixture", rev: 1, cursor: "fixture:1", views: { item: [], session: [], decision: [], message: { views: [], next: null } } })
  if (p === "/api/notifications") return json([])
  if (p === "/api/plugins/themes") return json({ themes: [] })
  if (p === "/api/me") return json({ user: { id: "u1", name: "Owner", email: null, role: "owner" }, version: "fixture", web_version: "fixture" })
  if (p === "/api/stream") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": keepalive\n\n"); return }

  if (p === "/api/file-deliveries" && req.method === "GET") {
    return json([...deliveries].reverse().map((d) => deliveryMeta(d)))
  }
  if (p === "/api/file-deliveries" && req.method === "POST") {
    const chunks = []; for await (const c of req) chunks.push(c)
    const body = Buffer.concat(chunks), name = u.searchParams.get("name") || "file.bin"
    const files = name.endsWith(".zip") ? unzipStore(body) : null
    const entrypoint = u.searchParams.get("entrypoint") || (files?.["index.html"] ? "index.html" : files?.["README.md"] ? "README.md" : null)
    const interactive = u.searchParams.get("interactive") === "1"
    const entryText = entrypoint && files?.[entrypoint] ? files[entrypoint].toString("utf8") : ""
    const scripts = /<script[\s>]/i.test(entryText)
    const grant = interactive ? randomBytes(32).toString("hex") : null
    const d = { id: `del_${++seq}`, title: u.searchParams.get("title") || name, name, mime: name.endsWith(".zip") ? "application/zip" : mimeOf(name), body, files, entrypoint, interactive, scripts, grant, created: Date.now() + seq }
    deliveries.push(d); byId.set(d.id, d)
    if (grant) grants.set(grant, d)
    return json(deliveryMeta(d))
  }
  const dMatch = /^\/api\/file-deliveries\/([^/]+)(.*)$/.exec(p)
  if (dMatch) {
    const d = byId.get(decodeURIComponent(dMatch[1])), rest = dMatch[2]
    if (!d) return json({ error: "not_found" }, 404)
    if (!rest && req.method === "GET") return json(deliveryMeta(d))
    if (!rest && req.method === "PATCH") {
      let raw = ""; for await (const c of req) raw += c
      const patch = JSON.parse(raw)
      if (typeof patch.interactive === "boolean") {
        if (d.grant) grants.delete(d.grant)
        d.interactive = patch.interactive
        d.grant = d.interactive ? randomBytes(32).toString("hex") : null
        if (d.grant) grants.set(d.grant, d)
      }
      return json(deliveryMeta(d))
    }
    if (rest === "/shares" && req.method === "GET") {
      return json([...shares].filter(([, s]) => s.id === d.id).map(([token, s]) => ({ token, url: `${base}/shares/${token}`, expires_at: s.expires_at, revoked_at: s.revoked_at })))
    }
    if (rest === "/shares" && req.method === "POST") {
      let raw = ""; for await (const c of req) raw += c
      const { expires_at } = JSON.parse(raw)
      const token = randomUUID()
      shares.set(token, { id: d.id, expires_at, revoked_at: null })
      return json({ token, url: `${base}/shares/${token}`, expires_at })
    }
    if (rest.startsWith("/content/")) {
      const rel = rest.slice(9), body = d.files ? d.files[rel] : d.body, mime = d.files ? mimeOf(rel) : d.mime
      if (!body) return json({ error: "not_found" }, 404)
      const range = req.headers.range
      if (range && /^bytes=(\d+)-(\d*)$/.test(range)) {
        const [, s0, s1] = /^bytes=(\d+)-(\d*)$/.exec(range)
        const start = Number(s0), end = s1 ? Math.min(Number(s1), body.length - 1) : body.length - 1
        res.writeHead(206, { "content-type": mime, "accept-ranges": "bytes", "content-range": `bytes ${start}-${end}/${body.length}`, "content-length": String(end - start + 1), "cache-control": "no-store" })
        return res.end(body.subarray(start, end + 1))
      }
      res.writeHead(200, { "content-type": mime, "accept-ranges": "bytes", "content-length": String(body.length), "cache-control": "no-store", ...(mime.startsWith("text/html") ? { "content-security-policy": staticHtmlCsp } : {}) })
      return res.end(body)
    }
    if (rest === "/raw") { res.writeHead(200, { "content-type": d.mime }); return res.end(d.body) }
  }
  return json({ error: "fixture" }, 404)
})
await new Promise((r) => hub.listen(0, "127.0.0.1", r))
const hubUrl = `http://127.0.0.1:${hub.address().port}`
const web = await createWebProxy({
  hubUrl,
  // Match steve-hub web.rs: delivery/share pages restrict embedded frames to this installation.
  responseHeaders(req) {
    const p = (req.url || "").split("?")[0]
    return p.startsWith("/deliveries/") || p.startsWith("/shares/") ? { "content-security-policy": "frame-src 'self'" } : null
  },
})
base = web.url

const probe = `
const out = {}, base = ${JSON.stringify(base)}, away = ${JSON.stringify(away)}
const attempt = async (name, f) => { try { out[name] = "allowed:" + String(await f()) } catch (e) { out[name] = "blocked:" + (e && e.name) } }
await attempt("origin", () => { if (self.origin !== "null") throw new Error(self.origin); return self.origin })
await attempt("cookie", () => document.cookie)
await attempt("localStorage", () => localStorage.length)
await attempt("sessionStorage", () => sessionStorage.length)
await attempt("indexedDB", () => new Promise((ok, no) => { const r = indexedDB.open("x"); r.onsuccess = () => ok("open"); r.onerror = () => no(r.error) }))
await attempt("parentDocument", () => parent.document.cookie)
await attempt("fetchApiRelative", () => fetch("/api/shell", { credentials: "include" }).then((r) => r.status))
await attempt("fetchApiAbsolute", () => fetch(base + "/api/me", { credentials: "include" }).then((r) => r.status))
await attempt("fetchOwnPackage", () => fetch("app.js").then((r) => r.status))
await attempt("fetchExternal", () => fetch(away + "/fetch", { mode: "no-cors" }).then((r) => r.type))
await attempt("xhrExternal", () => new Promise((ok, no) => { const x = new XMLHttpRequest(); x.open("GET", away + "/xhr"); x.onload = () => ok(x.status); x.onerror = () => no(new Error("xhr")); x.send() }))
await attempt("beacon", () => { if (!navigator.sendBeacon(away + "/beacon", "x")) throw new Error("refused"); return "queued" })
await attempt("websocket", () => new Promise((ok, no) => { const w = new WebSocket(away.replace("http", "ws") + "/ws"); w.onopen = () => ok("open"); w.onerror = () => no(new Error("ws")) }))
await attempt("eventSource", () => new Promise((ok, no) => { const s = new EventSource(away + "/sse"); s.onopen = () => ok("open"); s.onerror = () => { s.close(); no(new Error("sse")) } }))
await attempt("externalImage", () => new Promise((ok, no) => { const i = new Image(); i.onload = () => ok("loaded"); i.onerror = () => no(new Error("img")); i.src = away + "/img.png" }))
await attempt("apiImage", () => new Promise((ok, no) => { const i = new Image(); i.onload = () => ok("loaded"); i.onerror = () => no(new Error("img")); i.src = base + "/api/shell" }))
await attempt("externalScript", () => new Promise((ok, no) => { const s = document.createElement("script"); s.onload = () => ok("loaded"); s.onerror = () => no(new Error("script")); s.src = away + "/x.js"; document.head.append(s) }))
await attempt("worker", () => { new Worker("app.js"); return "started" })
await attempt("popup", () => { const w = window.open(away + "/popup"); if (!w) throw new Error("no window"); return "opened" })
await attempt("topNavigation", () => { top.location.href = away + "/top"; return "navigated" })
await attempt("form", () => { const f = document.createElement("form"); f.action = away + "/form"; f.method = "post"; document.body.append(f); f.submit(); return "submitted" })
await new Promise((r) => setTimeout(r, 800))
document.getElementById("results").textContent = JSON.stringify(out)
`
const prototype = {
  "index.html": `<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="style.css"><div id="app"></div><pre id="results"></pre>
<script src="app.js"></script><script type="module">${probe}</script>`,
  "app.js": `const app = document.getElementById("app"); let n = 0
const b = document.createElement("button"); b.id = "toggle"; b.textContent = "State 0"; b.onclick = () => { b.textContent = "State " + (++n) }
app.append(b); document.body.dataset.built = "yes"`,
  "style.css": "#toggle { color: rgb(12, 34, 56) }",
  "next.html": "<p id=next>next</p>",
}
const docs = { "README.md": "# Package docs\n\n![Bundled pixel](img/pixel.png)\n", "img/pixel.png": png }

const evidence = { base, away, checks: [], pageErrors: [] }
let browser, owner, page
const pause = (ms) => new Promise((r) => setTimeout(r, ms))
const shot = async (p, name) => { if (output) await p.screenshot({ path: `${output}/${name}.png`, fullPage: true }) }
async function check(name, f) {
  try { const detail = await f(); evidence.checks.push({ name, passed: true, detail }); console.log("PASS", name) }
  catch (e) { evidence.checks.push({ name, passed: false, error: String(e) }); if (page) await shot(page, "failure-" + evidence.checks.length).catch(() => {}); throw e }
}
async function publish(name, body, query = "") {
  const r = await owner.request.post(`${base}/api/file-deliveries?name=${encodeURIComponent(name)}${query}`, { headers: { "x-steve-ws": "default", "content-type": "application/octet-stream" }, data: body })
  assert.equal(r.status(), 200, await r.text())
  return r.json()
}
async function probes(frame) {
  await frame.locator("#results").filter({ hasText: "{" }).waitFor({ timeout: 15_000 })
  return JSON.parse(await frame.locator("#results").innerText())
}
function assertIsolated(out) {
  assert.equal(out.origin, "allowed:null", "opaque origin")
  for (const k of ["cookie", "localStorage", "sessionStorage", "indexedDB", "parentDocument", "fetchApiRelative", "fetchApiAbsolute", "fetchOwnPackage",
    "fetchExternal", "xhrExternal", "websocket", "eventSource", "externalImage", "apiImage", "externalScript", "worker", "popup", "topNavigation"]) {
    assert.match(out[k] ?? "missing", /^blocked:/, `${k}: ${out[k]}`)
  }
  assert.match(out.beacon ?? "missing", /^(blocked:|allowed:queued$)/)
  assert.match(out.form ?? "missing", /^(blocked:|allowed:submitted$)/)
}
try {
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
  browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
  owner = await browser.newContext({ locale: "zh-CN", viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
  owner.on("page", (p) => p.on("pageerror", (e) => evidence.pageErrors.push(e.message)))
  page = await owner.newPage()
  assert.equal((await owner.request.get(base + "/auth/dev")).status(), 200)
  const session = (await owner.cookies()).find((c) => c.name.startsWith("steve_session"))
  assert.ok(session, "the owner has a session cookie the frame must not reach")

  const stat = await publish("proto.zip", zip(prototype))
  await check("静态是默认：需要脚本的页面显示提示，而不是空白框", async () => {
    assert.equal(stat.interactive, false)
    assert.equal(stat.scripts, true)
    assert.equal(stat.frame_url, null)
    await page.goto(stat.url)
    await page.locator('[data-needs-scripts="empty"]').waitFor()
    assert.equal(await page.locator("[data-delivery-mode]").getAttribute("data-delivery-mode"), "static")
    assert.equal(await page.locator("[data-needs-scripts]").innerText(), "这个页面需要脚本才能显示，可开启交互预览。")
    assert.equal(await page.locator("[data-delivery-html]").getAttribute("sandbox"), "allow-same-origin")
    assert.equal(await page.locator("[data-delivery-html]").isVisible(), false, "the empty static frame is folded away")
    assert.equal(await page.frameLocator("[data-delivery-html]").locator("body").evaluate((b) => b.dataset.built ?? null), null, "no script ran")
    await shot(page, "static-default-notice")
    return "static default notice ok"
  })

  await check("所有者开关打开交互预览：iframe 只有 allow-scripts，脚本可交互", async () => {
    await page.locator("input[data-interactive-switch]").click()
    const iframe = page.locator("[data-delivery-interactive]")
    await iframe.waitFor()
    assert.equal(await iframe.getAttribute("sandbox"), "allow-scripts")
    const src = new URL(await iframe.getAttribute("src"))
    assert.match(src.pathname, /^\/api\/delivery-frames\/[0-9a-f]{64}\/index\.html$/)
    const frame = page.frameLocator("[data-delivery-interactive]")
    await frame.locator("#toggle").click()
    await frame.locator("#toggle").filter({ hasText: "State 1" }).waitFor()
    assert.equal(await frame.locator("#toggle").evaluate((e) => getComputedStyle(e).color), "rgb(12, 34, 56)", "package CSS")
    assert.equal(await page.locator("[data-delivery-mode]").getAttribute("data-delivery-mode"), "interactive")
    await shot(page, "interactive-owner")
    return { frame: src.pathname.replace(/[0-9a-f]{64}/, "<grant>") }
  })

  await check("交互 iframe 读不到 Steve cookie/存储，调不了 /api，也连不到外部地址", async () => {
    const out = await probes(page.frameLocator("[data-delivery-interactive]"))
    assertIsolated(out)
    await pause(500)
    assert.deepEqual(leaks, [], "the outside server received nothing")
    assert.equal(page.url(), stat.url, "no top navigation")
    assert.equal(owner.pages().length, 1, "no popup")
    return out
  })

  await check("iframe 把自己导航到外部站点会被交付页拦下", async () => {
    const frame = page.frames().find((f) => f.url().includes("/api/delivery-frames/"))
    assert.ok(frame)
    const refused = page.waitForEvent("console", { predicate: (m) => /frame-src/.test(m.text()) && m.text().includes(away), timeout: 10_000 })
    await frame.evaluate((to) => { location.href = to }, away + "/navigate").catch(() => {})
    const message = (await refused).text()
    await pause(500)
    assert.deepEqual(leaks, [], "frame-src keeps the frame on this installation")
    evidence.navigation_refusal = message
    return "frame-src refused self-navigation"
  })

  await check("frame 响应是不透明源且禁止连接；作为独立页面打开会被拒绝", async () => {
    const frameUrl = await page.locator("[data-delivery-interactive]").getAttribute("src")
    const r = await owner.request.get(frameUrl)
    const csp = r.headers()["content-security-policy"]
    for (const part of ["sandbox allow-scripts;", "connect-src 'none'", "form-action 'none'", `frame-ancestors 'self' ${base}`]) assert.ok(csp.includes(part), `${part}: ${csp}`)
    assert.ok(!csp.includes("allow-same-origin"))
    const direct = await owner.newPage()
    const opened = await direct.goto(frameUrl)
    assert.equal(opened.status(), 403, "a frame capability does not open as a page of its own")
    assert.equal(await direct.locator("#toggle").count(), 0)
    await direct.close()
    await pause(300)
    assert.deepEqual(leaks, [])
    return "direct frame navigation refused"
  })

  await check("匿名分享遵守同一开关：开着是隔离交互，关掉后 frame 立即 404", async () => {
    const r = await owner.request.post(`${base}/api/file-deliveries/${stat.id}/shares`, { headers: { "x-steve-ws": "default" }, data: { expires_at: Date.now() + 3_600_000 } })
    assert.equal(r.status(), 200)
    const share = await r.json()
    const c = await browser.newContext({ locale: "en-US", viewport: { width: 390, height: 844 } }), p = await c.newPage()
    await p.goto(share.url)
    const iframe = p.locator("[data-delivery-interactive]")
    await iframe.waitFor()
    assert.equal(await iframe.getAttribute("sandbox"), "allow-scripts")
    assert.match(new URL(await iframe.getAttribute("src")).pathname, new RegExp(`^/api/delivery-shares/${share.token}/frame/index\\.html$`))
    const frame = p.frameLocator("[data-delivery-interactive]")
    await frame.locator("#toggle").click()
    await frame.locator("#toggle").filter({ hasText: "State 1" }).waitFor()
    assertIsolated(await probes(frame))
    assert.equal(await p.locator("[data-interactive-switch]").count(), 0, "a share cannot switch it")
    await shot(p, "interactive-share-mobile")
    const frameUrl = await iframe.getAttribute("src")
    const off = await owner.request.patch(`${base}/api/file-deliveries/${stat.id}`, { headers: { "x-steve-ws": "default" }, data: { interactive: false } })
    assert.equal(off.status(), 200)
    assert.equal((await c.request.get(frameUrl)).status(), 404)
    await p.reload()
    await p.locator("[data-needs-scripts]").waitFor()
    await c.close()
    assert.deepEqual(leaks, [])
    return "share interactive toggle-off ok"
  })

  await check("发布时 --interactive；Markdown 相对图片、音频 range、我的发布列表", async () => {
    const live = await publish("live.zip", zip(prototype), "&interactive=1&title=Live")
    assert.equal(live.interactive, true)
    assert.ok(live.frame_url)
    const md = await publish("docs.zip", zip(docs))
    assert.equal(md.entrypoint, "README.md")
    await page.goto(md.url)
    await page.locator("[data-out-md]").waitFor()
    const img = page.locator('[data-out-md] img[alt="Bundled pixel"]')
    await img.waitFor()
    await page.waitForFunction(() => { const i = document.querySelector('[data-out-md] img[alt="Bundled pixel"]'); return i && i.complete && i.naturalWidth > 0 })
    assert.match(await img.getAttribute("src"), /\/api\/file-deliveries\/[^/]+\/content\/img\/pixel\.png\?ws=default$/)
    await shot(page, "markdown-package")
    const audio = await publish("tone.wav", wav())
    await page.goto(audio.url)
    await page.locator("[data-delivery-audio]").waitFor()
    await page.waitForFunction(() => { const a = document.querySelector("[data-delivery-audio]"); return a && a.readyState >= 1 && a.duration > 0 })
    const ranged = await owner.request.get(base + audio.content_url, { headers: { range: "bytes=0-11" } })
    assert.equal(ranged.status(), 206)
    assert.equal(ranged.headers()["content-range"], `bytes 0-11/${audio.size}`)
    assert.equal((await ranged.body()).subarray(0, 4).toString(), "RIFF")
    await page.goto(base + "/#/publications")
    await page.locator("[data-my-delivery]").first().waitFor()
    const ids = await page.locator("[data-my-delivery]").evaluateAll((rows) => rows.map((r) => r.getAttribute("data-my-delivery")))
    assert.deepEqual(ids, [audio.id, md.id, live.id, stat.id], "newest first, every own publication")
    await page.locator(`[data-my-delivery="${live.id}"] [data-my-delivery-shares]`).click()
    await page.locator(`[data-my-delivery="${live.id}"] [data-delivery-shares]`).waitFor()
    await shot(page, "my-publications")
    return { markdown_image: "package-relative, rendered", audio: "metadata decoded, range 206", list: ids.length }
  })

  assert.deepEqual(leaks, [], "nothing ever reached the outside server")
  assert.deepEqual(evidence.pageErrors, [])
} catch (e) {
  evidence.fatal = String(e)
  process.exitCode = 1
  if (page) await shot(page, "fatal").catch(() => {})
  console.error(e)
} finally {
  await browser?.close()
  outside.close()
  await web.close()
  hub.closeAllConnections()
  await new Promise((r) => hub.close(r))
  if (output) await writeFile(output + "/live-evidence.json", JSON.stringify(evidence, null, 2))
}
