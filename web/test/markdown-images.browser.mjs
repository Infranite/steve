/* Actual isolated Hub + production App/ChatView/Markdown. Synthetic messages and PNG, real dev login,
 * uploads and authorization. Requires STEVE_TEST_HUB_BIN, npm run build:web, Playwright and Chromium.
 * Only the explicit network-failure check and external-image control use browser routing.
 * Separate browser contexts exercise clients, not a second physical device or a remote Node. */
import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import { spawn, execFileSync } from "node:child_process"
import { once } from "node:events"
import { createReadStream, createWriteStream } from "node:fs"
import { access, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises"
import { createServer as reserveServer } from "node:net"
import { resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { createServer } from "vite"

const root = fileURLToPath(new URL("../..", import.meta.url)), binary = process.env.STEVE_TEST_HUB_BIN
assert.ok(binary, "STEVE_TEST_HUB_BIN must name a test executable; never connect to an existing Hub")
await access(binary); await access(root + "/web/dist/index.html")
await mkdir(root + "/.steve", { recursive: true })
const home = await mkdtemp(root + "/.steve/markdown-images-")
await writeFile(home + "/.image-browser-test", "synthetic markdown images\n")
const output = process.env.STEVE_TEST_SHOTS || home + "/evidence"
await mkdir(output, { recursive: true })
const reserve = reserveServer()
await new Promise((r) => reserve.listen(0, "127.0.0.1", r))
const port = reserve.address().port
await new Promise((r) => reserve.close(r))
const hubBase = `http://127.0.0.1:${port}`, previousHub = process.env.STEVE_HUB
process.env.STEVE_HUB = hubBase
const clientFileRequests = []
const web = await createServer({ root: root + "/web", configFile: root + "/web/vite.config.ts", logLevel: "error",
  server: { host: "127.0.0.1", port: 0, strictPort: true },
  plugins: [{ name: "standalone-image-client", configureServer(server) {
    // An independently hosted client does not serve the selected Hub's files itself.
    server.middlewares.use((req, res, next) => {
      if (!req.url.startsWith("/api/files/")) return next()
      clientFileRequests.push(req.url); res.statusCode = 404; res.end("client has no Hub files")
    })
  } }],
})
await web.listen()
const client = `http://127.0.0.1:${web.httpServer.address().port}`
const environment = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith("STEVE_")))
Object.assign(environment, { STEVE_HOME: home, STEVE_HOST: "127.0.0.1", STEVE_PORT: String(port), STEVE_PUBLIC_URL: hubBase,
  STEVE_DEV_LOGIN: "1", STEVE_REPO: home + "/no-source-checkout", STEVE_VAULT_KEYSTORE: "file", STEVE_FORBID_REAL_HOSTS: "1",
  STEVE_NO_RESUME: "1", STEVE_ADAPTER_CLAUDE: "/usr/bin/false", STEVE_ADAPTER_CODEX: "/usr/bin/false",
  STEVE_CLIENT_ORIGINS: client, STEVE_WEB_DIR: root + "/web/dist" })
const log = createWriteStream(home + "/hub.log", { mode: 0o600 })
const hub = spawn(resolve(binary), [], { env: environment, stdio: ["ignore", "pipe", "pipe"] })
hub.stdout.pipe(log, { end: false }); hub.stderr.pipe(log, { end: false })
const exited = once(hub, "exit"), hash = createHash("sha256")
for await (const chunk of createReadStream(binary)) hash.update(chunk)
const evidence = { revision: execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim(),
  diff_sha256: createHash("sha256").update(execFileSync("git", ["diff", "HEAD", "--", "web"], { cwd: root })).digest("hex"),
  hub_binary_sha256: hash.digest("hex"), hub_version: null, client, hub: hubBase,
  scope: "Actual isolated Hub, built App/ChatView and Vite Markdown fixture; synthetic DB message, authenticated API uploads. Independent browser contexts on one physical host. No Node/model/OAuth or production data.",
  checks: [], requests: [], pageErrors: [], injectedFailures: [], clientFileRequests }
let browser, page
const pause = (ms) => new Promise((r) => setTimeout(r, ms))
const fixture = (action, data) => execFileSync("python3", [root + "/web/test/fixtures/markdown-images.py", home, action], { input: data ? JSON.stringify(data) : undefined })
async function check(name, fn) {
  try { const detail = await fn(); evidence.checks.push({ name, passed: true, operated: true, detail }); console.log("PASS", name) }
  catch (e) { evidence.checks.push({ name, passed: false, operated: true, error: String(e) }); throw e }
}
const shot = (p, name) => p.screenshot({ path: `${output}/${name}.png`, fullPage: true })
async function decoded(p, selector) {
  await p.waitForFunction((s) => { const i = document.querySelector(s); return i?.complete && i.naturalWidth > 0 }, selector)
  return p.locator(selector).evaluate((i) => ({ src: i.src, naturalWidth: i.naturalWidth, naturalHeight: i.naturalHeight, alt: i.alt, crossOrigin: i.crossOrigin }))
}
async function noOverflow(p) {
  assert.equal(await p.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
  assert.deepEqual(await p.locator('[data-md-image] img').evaluateAll((images) => images.filter((i) => i.getBoundingClientRect().width > innerWidth).map((i) => i.alt)), [])
}
async function context(selected = true) {
  const c = await browser.newContext({ locale: "zh-CN", viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
  if (selected) await c.addInitScript((h) => { try { localStorage.setItem("steve.hub", h) } catch { /* about:blank and opaque sandbox frames have no storage */ } }, hubBase)
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
  for (let i = 0; ; i++) {
    assert.equal(hub.exitCode, null, "isolated Hub exited")
    const hello = await fetch(hubBase + "/auth/hello").then((r) => r.ok ? r.json() : null, () => null)
    if (hello) { evidence.hub_version = hello.version; break }
    assert.ok(i < 150, "isolated Hub failed to start"); await pause(200)
  }
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
    x.font = "24px sans-serif"; x.fillText("Synthetic fixture · no OAuth credentials", 24, 115)
    for (let y = 0; y < 4; y++) for (let a = 0; a < 14; a++) { x.fillStyle = (y + a) % 3 ? "#3a668a" : "#afc9dc"; x.fillRect(24 + a * 42, 150 + y * 20, 34, 12) }
    return c.toDataURL("image/png").split(",")[1]
  }), "base64")
  await writeFile(home + "/test-image.png", png)
  const uploaded = await owner.request.post(hubBase + "/api/files?name=synthetic-test.png", { headers: { "x-steve-ws": "default", "content-type": "application/octet-stream" }, data: png })
  assert.equal(uploaded.status(), 200); const file = await uploaded.json()
  fixture("package")
  const published = await owner.request.post(hubBase + "/api/file-deliveries?name=images.zip&entrypoint=docs%2FREADME.md", { headers: { "x-steve-ws": "default", "content-type": "application/octet-stream" }, data: await readFile(home + "/images.zip") })
  assert.equal(published.status(), 200, await published.text()); const delivery = await published.json()
  const image = `/api/files/${file.id}?ws=default&fixture=markdown%20image`
  fixture("seed", { markdown: `ST-158 合成图片验收（非授权二维码）\n\n![当前 Hub 测试图片](${image})\n\n![完整 Hub 测试图片](${hubBase}${image})\n\n![本地路径图片](/private/tmp/steve-image-test.png)` })
  const fixtureUrl = `${client}/test/fixtures/markdown-images.html?file=${file.id}&delivery=${delivery.id}`
  await check("真实 ChatView 的当前 Hub 与绝对 Hub 图片解码", async () => {
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
  await check("真实已鉴权交付包相对图与包内链接不退化", async () => {
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
  await check("另一独立客户端的真实 401、登录后点击重试恢复", async () => {
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
    assert.match(await p.locator('[data-case="missing"] [data-md-image-error]').innerText(), /Image failed to load/)
    await c.close(); return { local_and_unsafe_image_requests: 0, external_control: "unchanged", streaming_placeholder: true, english_failure: true }
  })
  await check("同源生产构建页面与第二浏览器上下文持续解码", async () => {
    const c = await context(false); await c.addCookies(await owner.cookies()); const p = await c.newPage()
    await c.request.post(hubBase + "/api/workspaces/default/switch", { headers: { "x-steve-ws": "image-other" }, data: {} })
    await p.goto(hubBase + "/#/")
    const i = await decoded(p, 'img[alt="当前 Hub 测试图片"]'); assert.equal(new URL(i.src).origin, hubBase)
    await p.reload(); await decoded(p, 'img[alt="完整 Hub 测试图片"]')
    await shot(p, "built-same-origin-chat"); await c.close(); return i
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
  await browser?.close(); await web.close()
  if (hub.exitCode === null) hub.kill("SIGTERM")
  await exited; log.end()
  await writeFile(output + "/browser-evidence.json", JSON.stringify(evidence, null, 2))
  if (previousHub === undefined) delete process.env.STEVE_HUB; else process.env.STEVE_HUB = previousHub
}
