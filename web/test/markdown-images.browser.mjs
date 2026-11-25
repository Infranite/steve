/* Isolated HTTP Hub fixture + Vite App/ChatView/Markdown: cross-origin credentialed image loading
 * (steve.hub), ?ws= precedence over active workspace cookie, package relative images, 404/401/network
 * retry recovery, local-path & unsafe-protocol blocking, and same-origin rendering. */
import assert from "node:assert/strict"
import { createServer as createHttp } from "node:http"
import { mkdir, writeFile } from "node:fs/promises"
import { createWebProxy } from "./fixtures/hub-proxy.mjs"

const output = process.env.STEVE_TEST_SHOTS
if (output) await mkdir(output, { recursive: true })

const clientFileRequests = []
const web = await createWebProxy({
  // An independently hosted client does not serve the selected Hub's files itself.
  onRequest(req, res) {
    if (!req.url?.startsWith("/api/files/")) return false
    clientFileRequests.push(req.url); res.statusCode = 404; res.end("client has no Hub files"); return true
  },
})
const client = web.url

const files = new Map(), deliveries = new Map()
let seededMarkdown = "", fileSeq = 0, deliverySeq = 0
const wsDefault = { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }
const wsOther = { id: "image-other", name: "其他", description: "", item_prefix: "OT", sort: 1, created: 0 }
let hubBase = ""

const hub = createHttp(async (req, res) => {
  const u = new URL(req.url, "http://127.0.0.1"), p = u.pathname
  const origin = req.headers.origin
  const cors = origin === client ? {
    "access-control-allow-origin": client,
    "access-control-allow-credentials": "true",
    "access-control-allow-methods": "GET, POST, PATCH, DELETE, OPTIONS",
    "access-control-allow-headers": "content-type, x-steve-ws",
    vary: "Origin",
  } : {}
  if (req.method === "OPTIONS") { res.writeHead(204, cors); return res.end() }
  const json = (v, status = 200, extra = {}) => {
    res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store", ...cors, ...extra })
    res.end(JSON.stringify(v))
  }
  const authed = (req.headers.cookie || "").includes("steve_session=fixture-auth")
  const cookieWs = /steve_ws=([^;]+)/.exec(req.headers.cookie || "")?.[1] || "default"
  const ws = req.headers["x-steve-ws"] || u.searchParams.get("ws") || cookieWs

  if (p === "/auth/hello") return json({ name: "steve", version: "fixture", api: 2, login: { token: false, oidc: false } })
  if (p === "/auth/dev") {
    res.writeHead(200, { "content-type": "application/json", "set-cookie": "steve_session=fixture-auth; HttpOnly; SameSite=Lax; Path=/", ...cors })
    return res.end('{"ok":true}')
  }
  if (p.startsWith("/api/")) {
    if (!authed) return json({ error: "unauthenticated" }, 401, { "cross-origin-resource-policy": "cross-origin" })
    if (p === "/api/files" && req.method === "POST") {
      const chunks = []; for await (const c of req) chunks.push(c)
      const id = `file_${++fileSeq}`, buf = Buffer.concat(chunks)
      files.set(id, { ws, name: u.searchParams.get("name") || "file.png", body: buf })
      return json({ id, name: u.searchParams.get("name") || "file.png", mime: "image/png", size: buf.length })
    }
    const fileMatch = /^\/api\/files\/([^/]+)$/.exec(p)
    if (fileMatch && req.method === "GET") {
      const f = files.get(decodeURIComponent(fileMatch[1]))
      if (!f || f.ws !== ws) return json({ error: "not_found" }, 404, { "cross-origin-resource-policy": "cross-origin" })
      res.writeHead(200, { "content-type": "image/png", "cache-control": "no-store", "cross-origin-resource-policy": "cross-origin", ...cors })
      return res.end(f.body)
    }
    if (p === "/api/file-deliveries" && req.method === "POST") {
      const chunks = []; for await (const c of req) chunks.push(c)
      const id = `del_${++deliverySeq}`, entry = u.searchParams.get("entrypoint") || null
      deliveries.set(id, { ws, entrypoint: entry, body: Buffer.concat(chunks) })
      return json({ id, workspace_id: ws, entrypoint: entry })
    }
    const delMatch = /^\/api\/file-deliveries\/([^/]+)\/content\/(.+)$/.exec(p)
    if (delMatch && req.method === "GET") {
      const d = deliveries.get(decodeURIComponent(delMatch[1])), rel = delMatch[2]
      if (!d || d.ws !== ws) return json({ error: "not_found" }, 404, { "cross-origin-resource-policy": "cross-origin" })
      if (rel === "assets/test.png") {
        res.writeHead(200, { "content-type": "image/png", "cache-control": "no-store", "cross-origin-resource-policy": "cross-origin", ...cors })
        return res.end(d.body)
      }
      if (rel === "docs/README.md" || rel === "about.md") {
        res.writeHead(200, { "content-type": "text/markdown", "cache-control": "no-store", "cross-origin-resource-policy": "cross-origin", ...cors })
        return res.end("# Package doc\n")
      }
      return json({ error: "not_found" }, 404, { "cross-origin-resource-policy": "cross-origin" })
    }
    const sw = /^\/api\/workspaces\/([^/]+)\/switch$/.exec(p)
    if (sw && req.method === "POST") {
      res.writeHead(200, { "content-type": "application/json", "set-cookie": `steve_ws=${sw[1]}; HttpOnly; SameSite=Lax; Path=/`, ...cors })
      return res.end('{"ok":true}')
    }
    if (p === "/api/shell") return json({
      version: "fixture", web_version: "fixture", owner: true,
      workspace: ws === "image-other" ? wsOther : wsDefault, workspaces: [wsDefault, wsOther],
      bots: [{ id: "steve", name: "Steve", role: "", main: true, harness: "claude-code", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default", "image-other"], memories: 0 }],
      allBots: [], projects: [], tags: [], settings: { onboard: "1" },
    })
    if (p === "/api/summary") return json({ ws, thinking: false, hold: null })
    if (p === "/api/snapshot") return json({
      epoch: "fixture", rev: 1, cursor: "fixture:1",
      views: {
        item: [], session: [], decision: [],
        message: {
          views: ws === "default" && seededMarkdown ? [{ id: "m_img", ws: "default", author: "steve", text: seededMarkdown, item_id: null, created: Date.now() - 1000, meta: {} }] : [],
          next: null,
        },
      },
    })
    if (p === "/api/notifications") return json([])
    if (p === "/api/conversations/channels") return json({ channels: [] })
    if (p === "/api/plugins/themes") return json({ themes: [] })
    if (p === "/api/me") return json({ user: { id: "u1", name: "Owner", email: null, role: "owner" }, version: "fixture", web_version: "fixture" })
    if (p === "/api/stream") { res.writeHead(200, { "content-type": "text/event-stream", ...cors }); res.write(": keepalive\n\n"); return }
    return json({ error: "fixture" }, 404)
  }
  // Proxy non-API page/asset requests to Vite so hubBase also serves the same-origin app.
  const upstream = await fetch(client + req.url)
  const headers = Object.fromEntries([...upstream.headers.entries()].filter(([k]) => !["content-encoding", "transfer-encoding", "content-length"].includes(k.toLowerCase())))
  res.writeHead(upstream.status, headers)
  res.end(Buffer.from(await upstream.arrayBuffer()))
})
await new Promise((r) => hub.listen(0, "127.0.0.1", r))
hubBase = `http://127.0.0.1:${hub.address().port}`

const evidence = { client, hub: hubBase, checks: [], requests: [], pageErrors: [], injectedFailures: [], clientFileRequests }
let browser, page
async function check(name, fn) {
  try { const detail = await fn(); evidence.checks.push({ name, passed: true, operated: true, detail }); console.log("PASS", name) }
  catch (e) { evidence.checks.push({ name, passed: false, operated: true, error: String(e) }); throw e }
}
const shot = async (p, name) => { if (output) await p.screenshot({ path: `${output}/${name}.png`, fullPage: true }) }
async function decoded(p, selector) {
  await p.waitForFunction((s) => { const i = document.querySelector(s); return i?.complete && i.naturalWidth > 0 }, selector)
  return p.locator(selector).evaluate((i) => ({ src: i.src, naturalWidth: i.naturalWidth, naturalHeight: i.naturalHeight, alt: i.alt, crossOrigin: i.crossOrigin }))
}
async function noOverflow(p) {
  assert.equal(await p.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
  assert.deepEqual(await p.locator("[data-md-image] img").evaluateAll((images) => images.filter((i) => i.getBoundingClientRect().width > innerWidth).map((i) => i.alt)), [])
}
async function context(selected = true) {
  const c = await browser.newContext({ locale: "zh-CN", viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
  if (selected) await c.addInitScript((h) => { try { localStorage.setItem("steve.hub", h) } catch { /* opaque frame */ } }, hubBase)
  c.on("page", (p) => {
    p.on("pageerror", (e) => evidence.pageErrors.push(e.message))
    p.on("console", (m) => { if (m.type() === "error" && /In HTML,|cannot contain a nested/.test(m.text())) evidence.pageErrors.push(m.text()) })
    p.on("response", async (r) => {
      const u = new URL(r.url())
      if (!/^\/api\/(files|file-deliveries)\//.test(u.pathname)) return
      const h = await r.request().allHeaders().catch(() => ({}))
      evidence.requests.push({ origin: u.origin, path: u.pathname, query: u.search, status: r.status(), cookie_present: !!h.cookie,
        workspace_header: h["x-steve-ws"] ?? null, mode: h["sec-fetch-mode"], corp: r.headers()["cross-origin-resource-policy"] ?? null })
    })
  })
  return c
}
try {
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
  browser = await chromium.launch({ headless: true,
    ...(process.env.PLAYWRIGHT_CHANNEL ? { channel: process.env.PLAYWRIGHT_CHANNEL } : {}),
    ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
  const owner = await context()
  page = await owner.newPage()
  assert.equal((await owner.request.get(hubBase + "/auth/dev")).status(), 200)
  const png = Buffer.from(await page.evaluate(() => {
    const c = document.createElement("canvas"); c.width = 640; c.height = 260
    const x = c.getContext("2d"); x.fillStyle = "#e8f0fb"; x.fillRect(0, 0, 640, 260)
    x.fillStyle = "#1c3e6e"; x.font = "bold 32px sans-serif"; x.fillText("Steve · authenticated test image", 24, 68)
    return c.toDataURL("image/png").split(",")[1]
  }), "base64")
  const uploaded = await owner.request.post(hubBase + "/api/files?name=synthetic-test.png", { headers: { "x-steve-ws": "default", "content-type": "application/octet-stream" }, data: png })
  assert.equal(uploaded.status(), 200); const file = await uploaded.json()
  const published = await owner.request.post(hubBase + "/api/file-deliveries?name=images.zip&entrypoint=docs%2FREADME.md", { headers: { "x-steve-ws": "default", "content-type": "application/octet-stream" }, data: png })
  assert.equal(published.status(), 200); const delivery = await published.json()
  const image = `/api/files/${file.id}?ws=default&fixture=markdown%20image`
  seededMarkdown = `ST-158 合成图片验收\n\n![当前 Hub 测试图片](${image})\n\n![完整 Hub 测试图片](${hubBase}${image})\n\n![本地路径图片](/private/tmp/steve-image-test.png)`
  const fixtureUrl = `${client}/test/fixtures/markdown-images.html?file=${file.id}&delivery=${delivery.id}`

  await check("ChatView 的当前 Hub 与绝对 Hub 图片解码", async () => {
    await page.goto(client + "/#/")
    const images = []
    for (const alt of ["当前 Hub 测试图片", "完整 Hub 测试图片"]) {
      const i = await decoded(page, `img[alt="${alt}"]`)
      assert.equal(new URL(i.src).origin, hubBase); assert.equal(i.naturalWidth, 640); assert.equal(i.crossOrigin, "use-credentials"); images.push(i)
    }
    assert.equal(clientFileRequests.length, 0); await shot(page, "conversation-desktop")
    return images
  })
  await check("刷新及 390px 窄屏对话图片持续显示", async () => {
    await page.reload(); await decoded(page, 'img[alt="当前 Hub 测试图片"]')
    await page.setViewportSize({ width: 390, height: 844 }); await noOverflow(page)
    await shot(page, "conversation-mobile-390")
    return decoded(page, 'img[alt="完整 Hub 测试图片"]')
  })
  await check("URL 工作区优先于当前页面及其他标签页的工作区", async () => {
    assert.equal((await owner.request.post(hubBase + "/api/workspaces/image-other/switch", { headers: { "x-steve-ws": "default" }, data: {} })).status(), 200)
    await page.goto(fixtureUrl)
    const images = []
    for (const name of ["relative", "reference", "absolute"]) {
      const i = await decoded(page, `[data-case="${name}"] img`), u = new URL(i.src)
      assert.deepEqual(u.searchParams.getAll("ws"), ["default"]); assert.equal(u.searchParams.get("fixture"), "markdown image"); assert.equal(u.hash, "#preview"); images.push(i)
    }
    const wrong = await owner.request.get(hubBase + `/api/files/${file.id}?ws=image-other`)
    assert.equal(wrong.status(), 404)
    return { active_page_and_cookie: "image-other", source: "default", images, wrong_workspace_status: wrong.status() }
  })
  await check("已鉴权交付包相对图与包内链接不退化", async () => {
    const i = await decoded(page, '[data-case="package"] img'), u = new URL(i.src)
    assert.equal(u.pathname, `/api/file-deliveries/${delivery.id}/content/assets/test.png`); assert.equal(u.search, "?ws=default")
    await page.locator('[data-case="package"]').getByRole("button", { name: "包内说明" }).click()
    assert.ok((await page.locator('[data-streamdown="link-safety-modal"]').innerText()).includes(`${hubBase}/api/file-deliveries/${delivery.id}/content/about.md?ws=default`))
    await page.locator('[data-streamdown="link-safety-modal"]').getByRole("button", { name: "Close", exact: true }).click()
    await decoded(page, '[data-case="mixed"] img')
    await noOverflow(page); await shot(page, "markdown-contract-mobile")
    return i
  })
  await check("404 中文提示、alt 保留和源地址变更后恢复", async () => {
    const missing = page.locator('[data-case="missing"] [data-md-image-error]')
    await missing.waitFor(); assert.match(await missing.innerText(), /缺失图片.*图片加载失败/s)
    await missing.getByRole("button", { name: "重试", exact: true }).click(); await missing.waitFor()
    await page.getByRole("button", { name: "Change image source" }).click()
    const i = await decoded(page, '[data-case="change"] img')
    assert.equal(await page.locator('[data-case="change"] [data-md-image-error]').count(), 0)
    return i
  })
  await check("临时网络失败后的手动重试恢复且不更改 ws", async () => {
    const c = await context(); await c.addCookies(await owner.cookies()); const p = await c.newPage()
    await p.route(hubBase + `/api/files/${file.id}?**`, (route) => { evidence.injectedFailures.push({ kind: "connectionfailed", path: new URL(route.request().url()).pathname }); return route.abort("connectionfailed") })
    await p.goto(fixtureUrl)
    const failure = p.locator('[data-case="relative"] [data-md-image-error]'); await failure.waitFor()
    await shot(p, "network-failure-chinese")
    await p.unrouteAll()
    await failure.getByRole("button", { name: "重试", exact: true }).click()
    const i = await decoded(p, '[data-case="relative"] img'); assert.equal(new URL(i.src).searchParams.get("ws"), "default")
    await shot(p, "network-recovered"); await c.close(); return i
  })
  await check("另一独立客户端的 401、登录后点击重试恢复", async () => {
    const c = await context(), p = await c.newPage(), start = evidence.requests.length
    await p.goto(fixtureUrl)
    const failure = p.locator('[data-case="relative"] [data-md-image-error]'); await failure.waitFor()
    assert.ok(evidence.requests.slice(start).some((r) => r.path === `/api/files/${file.id}` && r.status === 401 && !r.cookie_present))
    await shot(p, "signed-out-chinese")
    assert.equal((await c.request.get(hubBase + "/auth/dev")).status(), 200)
    await failure.getByRole("button", { name: "重试", exact: true }).click()
    const i = await decoded(p, '[data-case="relative"] img')
    await shot(p, "signed-in-recovered"); await c.close(); return i
  })
  await check("本地路径与危险协议不请求，HTML 及代码示例不执行", async () => {
    const requested = [], c = await context(); await c.addCookies(await owner.cookies()); const p = await c.newPage()
    await p.route("https://images.example.test/**", (r) => r.fulfill({ contentType: "image/png", body: png }))
    p.on("request", (r) => requested.push(r.url()))
    await p.goto(fixtureUrl); await decoded(p, '[data-case="relative"] img'); await decoded(p, '[data-case="external"] img')
    assert.equal(await p.locator('[data-case="external"] img').getAttribute("crossorigin"), null)
    assert.equal(await p.locator('[data-case="local"] img, [data-case="unsafe"] img').count(), 0)
    assert.equal(requested.some((r) => /private\/tmp|generated\/test|javascript:|file:\/\//.test(r)), false)
    assert.equal(await p.locator('a[href^="javascript:"]').count(), 0)
    assert.equal(await p.evaluate(() => window.markdownImageExecuted), undefined)
    assert.equal(await p.locator('[data-case="streaming"] [data-streamdown="image-placeholder"]').count(), 1)
    assert.equal(await p.locator('[data-case="local"] button').count(), 0)
    await p.getByRole("button", { name: "Switch to English" }).click()
    await p.waitForFunction(() => document.documentElement.lang === "en")
    assert.match(await p.locator('[data-case="missing"] [data-md-image-error]').innerText(), /Image failed to load/)
    await c.close(); return { local_and_unsafe_image_requests: 0, external_control: "unchanged", streaming_placeholder: true, english_failure: true }
  })
  await check("同源页面与第二浏览器上下文持续解码", async () => {
    const c = await context(false); await c.addCookies(await owner.cookies()); const p = await c.newPage()
    await c.request.post(hubBase + "/api/workspaces/default/switch", { headers: { "x-steve-ws": "image-other" }, data: {} })
    await p.goto(hubBase + "/#/")
    const i = await decoded(p, 'img[alt="当前 Hub 测试图片"]'); assert.equal(new URL(i.src).origin, hubBase)
    await p.reload(); await decoded(p, 'img[alt="完整 Hub 测试图片"]')
    await shot(p, "same-origin-chat"); await c.close(); return i
  })
  await check("无页面异常、无 HTML 嵌套告警、无客户端原点误读图片", () => {
    assert.deepEqual(evidence.pageErrors, []); assert.deepEqual(clientFileRequests, [])
    return { pageErrors: 0, invalid_nesting: 0, client_file_requests: 0 }
  })
} catch (e) {
  evidence.error = String(e)
  if (page) { await shot(page, "failure").catch(() => {}); console.error((await page.locator("body").innerText()).slice(0, 2400)) }
  throw e
} finally {
  await browser?.close()
  await web.close()
  hub.closeAllConnections()
  await new Promise((r) => hub.close(r))
  if (output) await writeFile(output + "/browser-evidence.json", JSON.stringify(evidence, null, 2))
}
