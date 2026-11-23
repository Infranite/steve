/* Real Hub integration. Requires npm run build:web, STEVE_TEST_HUB_BIN and Playwright.
 * Starts its own empty, loopback-only installation with no Node/model backend or real credentials.
 * Production MCP -> artifacts -> thread -> ChatView; no injected API payloads or synthetic card identity.
 * The item/session/origin records and source files are synthetic; this is not remote Node/device acceptance. */
import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import { spawn, execFileSync } from "node:child_process"
import { once } from "node:events"
import { constants, createReadStream, createWriteStream } from "node:fs"
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { createServer } from "node:net"
import { tmpdir } from "node:os"
import { resolve } from "node:path"
import { fileURLToPath } from "node:url"

const root = fileURLToPath(new URL("../..", import.meta.url))
const binary = process.env.STEVE_TEST_HUB_BIN
assert.ok(binary, "STEVE_TEST_HUB_BIN must name the test Hub executable; an existing Hub is never used")
await access(resolve(binary), constants.X_OK)
const hash = createHash("sha256")
for await (const part of createReadStream(resolve(binary))) hash.update(part)
const binaryHash = hash.digest("hex")
await readFile(resolve(root, "web/dist/index.html"))
const home = await mkdtemp(resolve(tmpdir(), "steve-delivery-browser-"))
await writeFile(home + "/.delivery-browser-test", "isolated delivery browser test\n")
const output = process.env.STEVE_TEST_SHOTS || home + "/evidence"
await mkdir(output, { recursive: true })
const reserve = createServer()
await new Promise((r) => reserve.listen(0, "127.0.0.1", r))
const port = reserve.address().port
await new Promise((r) => reserve.close(r))
const base = `http://127.0.0.1:${port}`
const environment = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("STEVE_")))
Object.assign(environment, { HOME: home, STEVE_HOME: home, STEVE_HOST: "127.0.0.1", STEVE_PORT: String(port), STEVE_PUBLIC_URL: base,
  STEVE_DEV_LOGIN: "1", STEVE_REPO: resolve(home, "no-source-checkout"), STEVE_VAULT_KEYSTORE: "file", STEVE_FORBID_REAL_HOSTS: "1",
  STEVE_NO_RESUME: "1", STEVE_ADAPTER_CLAUDE: "/usr/bin/false", STEVE_ADAPTER_CODEX: "/usr/bin/false",
  STEVE_ADAPTER_PI: "/usr/bin/false", STEVE_BACKUP: "off", STEVE_ADAPTER_UPDATES: "off", STEVE_WEB_DIR: resolve(root, "web/dist") })
const log = createWriteStream(home + "/hub.log", { mode: 0o600 })
const hub = spawn(resolve(binary), [], { env: environment, stdio: ["ignore", "pipe", "pipe"] })
hub.stdout.pipe(log, { end: false }); hub.stderr.pipe(log, { end: false })
const exited = once(hub, "exit")
const evidence = { frontend_revision: execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim(),
  frontend_diff_sha256: createHash("sha256").update(execFileSync("git", ["diff", "HEAD", "--", "web"], { cwd: root })).digest("hex"),
  backend_revision: process.env.STEVE_TEST_BACKEND_REV || "unspecified", binary_sha256: binaryHash, base,
  scope: "Fresh isolated Hub, actual MCP/tools/APIs and built production frontend; synthetic item/session/files, same physical host, local file receive (Node off). No API fixtures or injected cards. No production services.",
  checks: [], requests: [], pageErrors: [] }
let browser, owner, page
const pause = (ms) => new Promise((r) => setTimeout(r, ms))
const fixture = (action) => execFileSync("python3", [resolve(root, "web/test/fixtures/delivery-hub.py"), home, action], { stdio: "pipe" })
const shot = async (p, name) => p.screenshot({ path: `${output}/${name}.png`, fullPage: true })
async function check(name, f) {
  try { const detail = await f(); evidence.checks.push({ name, passed: true, operated: true, detail }); console.log("PASS", name) }
  catch (e) { evidence.checks.push({ name, passed: false, operated: true, error: String(e) }); await shot(page, "failure-" + evidence.checks.length).catch(() => {}); throw e }
}
async function tool(ws, name, args) {
  const token = await readFile(`${home}/${ws}.token`, "utf8")
  const r = await fetch(base + "/mcp", { method: "POST", headers: { authorization: "Bearer " + token, "content-type": "application/json", accept: "application/json, text/event-stream", "mcp-protocol-version": "2025-06-18" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }) })
  assert.equal(r.status, 200)
  const v = await r.json()
  assert.equal(v.error, undefined, JSON.stringify(v.error))
  assert.notEqual(v.result?.isError, true, JSON.stringify(v.result))
  const text = v.result.content.filter((c) => c.type === "text").map((c) => c.text).join("\n")
  return name === "steve_deliver_file" ? JSON.parse(text) : text
}
async function json(path, ws) {
  const r = await owner.request.get(base + path, { headers: ws ? { "x-steve-ws": ws } : {} })
  assert.equal(r.status(), 200, path)
  return r.json()
}
async function staticPreview(p, marker, navigate = false) {
  const f = p.frameLocator("[data-delivery-html]")
  await f.locator("#marker").filter({ hasText: marker }).waitFor()
  assert.equal(await p.locator("[data-delivery-html]").getAttribute("sandbox"), "allow-same-origin")
  assert.equal(await f.locator("#marker").evaluate((e) => getComputedStyle(e).color), "rgb(12, 34, 56)")
  assert.equal(await f.locator("#picture").evaluate((e) => e.complete && e.naturalWidth > 0), true)
  assert.equal(await f.locator("body").evaluate(() => window.executed), undefined)
  await f.locator("#interaction").click()
  assert.equal(await f.locator("#interaction").innerText(), "Run script")
  assert.equal(await p.locator("body").getAttribute("data-compromised"), null)
  if (navigate) {
    await f.locator("#next").click()
    await f.locator("#marker").filter({ hasText: "NEXT " + marker }).waitFor()
    assert.equal(await f.locator("#marker").evaluate((e) => getComputedStyle(e).color), "rgb(12, 34, 56)")
    assert.equal(await f.locator("#picture").evaluate((e) => e.complete && e.naturalWidth > 0), true)
  }
}
try {
  for (let i = 0; ; i++) {
    if (hub.exitCode !== null) throw new Error(`isolated Hub exited (${hub.exitCode})\n${await readFile(home + "/hub.log", "utf8")}`)
    if (await fetch(base + "/auth/hello").then((r) => r.ok, () => false)) break
    assert.ok(i < 150, "isolated Hub did not become ready")
    await pause(200)
  }
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
  browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
  owner = await browser.newContext({ locale: "zh-CN", viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
  owner.on("page", (p) => {
    p.on("pageerror", (e) => evidence.pageErrors.push(e.message))
    p.on("response", async (r) => {
      const u = new URL(r.url())
      if (!/^\/api\/(file-deliver|deliver|shell|items)/.test(u.pathname)) return
      const h = await r.request().allHeaders().catch(() => ({}))
      evidence.requests.push({ path: u.pathname.replace(/(delivery-shares\/)[^/]+/, "$1<token>"), query: u.search, status: r.status(), workspace: h["x-steve-ws"] ?? null, cookie_present: !!h.cookie, csp: r.headers()["content-security-policy"] ?? null })
    })
  })
  page = await owner.newPage()
  assert.equal((await owner.request.get(base + "/auth/dev")).status(), 200)
  await page.goto(base + "/deliveries/initializing")
  await page.locator("[data-delivery-error]").waitFor()
  fixture("seed")
  const receipts = {}
  await check("真实文件工具 → steve_deliver → artifacts.delivery_id → Hub生产聊天卡片", async () => {
    const files = [["md", "report.md", "交付报告"], ["html", "design.zip", "交互原型"], ["image", "pixel.png", "界面截图"], ["pdf", "paper.pdf", "验收清单"]]
    for (const [key, path, title] of files) receipts[key] = await tool("default", "steve_deliver_file", { path, title, ...(key === "html" ? { entrypoint: "pages/index.html" } : {}) })
    const summary = "本轮完成对话中的事项卡片更新，桌面和窄屏下都可以先读简短的交付摘要，再按需打开附件。交付包含验收报告、交互原型、界面截图和验收清单，展开后可直接打开、预览或下载。原有操作和交付记录保持不变。\n\n**完整验收说明**：分别验证仅展开摘要、仅展开详情、两者同时展开，以及收起后恢复到默认状态。[事项记录](#/items/ST-1)"
    await tool("default", "steve_deliver", { summary, outputs: files.map(([key, , title]) => ({ kind: "file", title, location: receipts[key].url })), checks: [{ name: "Actual tool returned received immutable files", passed: true }] })
    const detail = await json("/api/items/ST-1", "default")
    evidence.artifacts = detail.artifacts
    for (const d of Object.values(receipts)) assert.ok(detail.artifacts.some((a) => a.delivery_id === d.id && a.location === d.url), "actual artifact missing delivery_id")
    const messages = await json("/api/views/page?kind=message", "default")
    assert.ok(messages.views.some((m) => m.meta?.card?.item === "ST-1"), "Hub must generate the chat card")
    return "Four actual tool receipts, exact matching API artifacts and Hub-generated meta.card; no fixture delivery_id/card injection."
  })
  for (const [width, touch] of [[1280, false], [390, true]]) {
    const context = touch ? await browser.newContext({ locale: "zh-CN", viewport: { width, height: 844 }, reducedMotion: "reduce", isMobile: true, hasTouch: true }) : owner
    if (touch) await context.addCookies(await owner.cookies())
    const chat = touch ? await context.newPage() : page
    if (touch) chat.on("pageerror", (e) => evidence.pageErrors.push(e.message))
    try {
      await check(`实际操作 ${width}px 聊天事项卡：两处独立折叠，打开、预览及下载四条交付中的文件`, async () => {
        const baseCard = chat.locator('[data-item-card="ST-1"]')
        const summary = baseCard.locator("[data-card-summary]"), expand = baseCard.locator("[data-card-expand]")
        const details = baseCard.getByRole("button", { name: "交付详情 · 4 条", exact: true })
        const rows = baseCard.locator("[data-delivery-card]")
        const full = summary.getByText("完整验收说明", { exact: true })
        const operate = (locator) => touch ? locator.tap() : locator.click()
        const state = async (s, d) => {
          assert.equal(await expand.getAttribute("aria-expanded"), String(s), "summary expansion")
          assert.equal(await details.getAttribute("aria-expanded"), String(d), "details expansion")
          await rows.first().waitFor({ state: d ? "visible" : "hidden" })
        }
        const noOverflow = () => baseCard.evaluate((el) => ({
          page: document.documentElement.scrollWidth <= innerWidth,
          card: el.scrollWidth <= el.clientWidth,
        })).then((result) => assert.deepEqual(result, { page: true, card: true }))
        const capture = (name) => baseCard.screenshot({ path: `${output}/chat-card-${width}-${name}.png` })

        await chat.goto(base + "/#/")
        await details.waitFor()
        await state(false, false)
        assert.match(await summary.innerText(), /^本轮完成对话中的事项卡片更新/)
        assert.ok(await summary.evaluate((el) => el.clientHeight <= 2 * parseFloat(getComputedStyle(el).lineHeight) + 1), "preview fits in two lines")
        assert.equal(await full.count(), 0)
        assert.equal(await summary.getByRole("link").count(), 0, "no focusable links in the preview")
        assert.ok(await baseCard.locator("[data-pass]").isVisible(), "acceptance stays outside both disclosures")
        await noOverflow()
        await capture("default")

        if (touch) await operate(expand)
        else await expand.press("Enter")
        await full.waitFor()
        await summary.getByRole("link", { name: "事项记录" }).waitFor()
        await state(true, false)
        await capture("summary")
        await operate(details)
        await state(true, true)
        await operate(expand)
        await state(false, true)
        assert.equal(await full.count(), 0)
        assert.equal(await rows.count(), 4)
        for (const d of Object.values(receipts)) {
          const row = baseCard.locator(`[data-delivery-card="${d.id}"]`)
          for (const action of ["open", "preview", "download"]) assert.equal(await row.locator(`[data-delivery-${action}]`).count(), 1)
        }
        await noOverflow()
        await details.scrollIntoViewIfNeeded()
        await shot(chat, `chat-card-${width}-details`)
        if (touch) {
          await rows.last().scrollIntoViewIfNeeded()
          await shot(chat, `chat-card-${width}-details-scrolled`)
        }

        const card = baseCard.locator(`[data-delivery-card="${receipts.md.id}"]`)
        const opened = chat.waitForEvent("popup")
        await operate(card.locator("[data-delivery-open]"))
        const popup = await opened
        await popup.locator("[data-out-md]").waitFor()
        await popup.close()
        const downloaded = chat.waitForEvent("download")
        await operate(card.locator("[data-delivery-download]"))
        assert.match(await readFile(await (await downloaded).path(), "utf8"), /Immutable \*\*Markdown\*\*/)
        await operate(card.locator("[data-delivery-preview]"))
        const preview = chat.locator("[data-preview]")
        await preview.locator("[data-out-md]").waitFor()
        await shot(chat, `chat-card-${width}-preview`)
        if (touch) await operate(preview.getByRole("button", { name: "关闭", exact: true }))
        else await chat.keyboard.press("Escape")
        await preview.waitFor({ state: "hidden" })

        await operate(expand)
        await operate(details)
        await state(true, false)
        await operate(expand)
        await state(false, false)
        return "Production ChatView: counted row only by default, independent summary/full and details states; all four file cards kept their actions. Opened the Markdown page, previewed it in place, and downloaded the original bytes."
      })
    } finally { if (touch) await context.close() }
  }
  await check("真实切换到工作区B后，用工具给出的A原样URL打开并点击包内链接", async () => {
    const switched = await owner.request.post(base + "/api/workspaces/delivery-b/switch", { headers: { "x-steve-ws": "default" }, data: {} })
    assert.equal(switched.status(), 200)
    assert.equal((await json("/api/shell")).workspace.id, "delivery-b")
    const original = receipts.html.url
    const at = evidence.requests.length
    await page.goto(original)
    await staticPreview(page, "default", true)
    const calls = evidence.requests.slice(at)
    const meta = calls.find((r) => r.path === `/api/file-deliveries/${receipts.html.id}`)
    assert.ok(meta)
    assert.ok(calls.some((r) => r.path === "/api/shell" && r.workspace === "default"))
    await page.locator("[data-loopback-warning]").first().waitFor()
    await shot(page, "workspace-b-original-a-url")
    const unqualified = new URL(original)
    unqualified.searchParams.delete("ws")
    await page.goto(unqualified.href)
    await page.locator("[data-delivery-error]").waitFor()
    assert.equal(await page.locator("[data-delivery-html]").count(), 0)
    return { current_workspace_before_open: "delivery-b", original_tool_url: original, first_metadata: meta, bound_shell_workspace: "default", missing_workspace_refused: true }
  })
  await check("当前工作区B的未登录浏览器完成登录回跳，A原样URL仍可打开", async () => {
    const wsCookies = (await owner.cookies()).filter((c) => c.value === "delivery-b")
    assert.ok(wsCookies.length > 0, "use the actual workspace cookie set by switch, never an assumed name")
    const c = await browser.newContext({ locale: "en-US" })
    await c.addCookies(wsCookies)
    const p = await c.newPage()
    const original = receipts.html.url
    await p.goto(original)
    await p.locator("[data-login]").click()
    await staticPreview(p, "default", true)
    assert.equal(p.url(), original)
    await shot(p, "login-return-with-workspace-b")
    await c.close()
    return "Used the actual B workspace cookie, signed in through development login, and returned to the original A URL with its workspace selector."
  })
  await check("当前工作区B下从交付页进入事项产出并实际预览HTML", async () => {
    await page.goto(receipts.html.url)
    await page.locator("[data-delivery-item]").click()
    await page.locator("[data-now]").waitFor()
    await page.getByRole("tab", { name: "产出", exact: true }).click()
    const artifact = evidence.artifacts.find((a) => a.delivery_id === receipts.html.id)
    await page.locator(`[data-out-id="${artifact.id}"]`).click()
    await staticPreview(page, "default", true)
    await shot(page, "real-item-output-preview")
    await page.keyboard.press("Escape")
    return "The workspace-bound item link and actual item output row opened the same Hub-owned static HTML, including relative assets and next page."
  })
  await check("登录HTML和匿名分享均为静态预览；CSS、图片、跳转可用，脚本及onclick不执行", async () => {
    await page.goto(receipts.html.url)
    await staticPreview(page, "default")
    await page.locator("[data-share-duration]").click()
    await page.getByRole("option", { name: "1 小时", exact: true }).click()
    await page.locator("[data-share-create]").click()
    await page.locator("[data-share-url]").waitFor()
    const url = await page.locator("[data-share-url]").getAttribute("href"), token = new URL(url).pathname.split("/").pop()
    const c = await browser.newContext({ locale: "en-US", viewport: { width: 390, height: 844 } }), p = await c.newPage()
    const calls = []
    p.on("request", (r) => { if (new URL(r.url()).pathname.startsWith("/api/")) calls.push(new URL(r.url()).pathname) })
    await p.goto(url)
    await staticPreview(p, "default", true)
    assert.ok(calls.every((path) => path.startsWith(`/api/delivery-shares/${token}`)), "anonymous entry must only use share APIs")
    const meta = await (await c.request.get(base + "/api/delivery-shares/" + token)).json()
    assert.ok(meta.expires_at == null || meta.expires_at > Date.now())
    for (const key of ["download_url", "content_url"]) assert.match(new URL(meta[key], base).pathname, new RegExp(`^/api/delivery-shares/${token}/`))
    const response = await c.request.get(meta.content_url)
    assert.equal(response.status(), 200)
    assert.match(response.headers()["content-security-policy"], /sandbox allow-same-origin;/)
    assert.match(response.headers()["content-security-policy"], /script-src 'none'/)
    await shot(p, "anonymous-static-share-mobile")
    const downloaded = p.waitForEvent("download")
    await p.locator("[data-delivery-download]").click()
    assert.ok((await readFile(await (await downloaded).path())).subarray(0, 2).equals(Buffer.from("PK")))
    await page.locator("[data-share-revoke]").first().click()
    await page.locator('[data-share-state="revoked"]').waitFor()
    for (const suffix of ["", "/raw", "/content/pages/index.html", "/content/assets/theme.css", "/content/assets/pixel.png"]) assert.equal((await c.request.get(base + "/api/delivery-shares/" + token + suffix)).status(), 404)
    await p.reload()
    await p.locator("[data-delivery-error]").waitFor()
    await shot(p, "anonymous-share-revoked")
    await c.close()
    return "User selected expiry and created share; anonymous static assets/navigation and original ZIP download worked; UI revocation denied metadata/raw/HTML/CSS/image immediately."
  })
  await check("显式错误工作区404、移除真实membership后409，不显示交付内容", async () => {
    const wrong = new URL(receipts.html.url); wrong.searchParams.set("ws", "delivery-b")
    assert.equal((await owner.request.get(base + `/api/file-deliveries/${receipts.html.id}`, { headers: { "x-steve-ws": "delivery-b" } })).status(), 404)
    await page.goto(wrong.href)
    await page.locator("[data-delivery-error]").waitFor()
    assert.equal(await page.locator("[data-delivery-html]").count(), 0)
    fixture("remove-membership")
    assert.equal((await owner.request.get(receipts.html.content_url)).status(), 409)
    await page.goto(receipts.html.url)
    await page.locator("[data-delivery-error]").waitFor()
    assert.equal(await page.locator("[data-delivery-html]").count(), 0)
    await shot(page, "membership-denied")
    fixture("restore-membership")
    return "No fallback to another workspace: explicit B selector returned 404, A membership removal returned 409 for metadata/resources and no iframe."
  })
  await check("到期分享的真实metadata、原文件及相对资源立即拒绝", async () => {
    const expires = Date.now() + 3000
    const r = await owner.request.post(base + `/api/file-deliveries/${receipts.html.id}/shares`, { headers: { "x-steve-ws": "default" }, data: { expires_at: expires } })
    assert.equal(r.status(), 200)
    const share = await r.json(), c = await browser.newContext(), p = await c.newPage()
    assert.equal(share.expires_at, expires)
    await p.goto(share.url)
    await p.frameLocator("[data-delivery-html]").locator("#marker").waitFor()
    await pause(Math.max(0, expires - Date.now() + 100))
    for (const suffix of ["", "/raw", "/content/pages/index.html", "/content/assets/theme.css"]) assert.equal((await c.request.get(base + "/api/delivery-shares/" + share.token + suffix)).status(), 404)
    await p.reload()
    await p.locator("[data-delivery-error]").waitFor()
    await shot(p, "anonymous-share-expired")
    await c.close()
    return "A real short-lived share loaded anonymously before its deadline; metadata/raw/HTML/CSS returned 404 after it and the page showed share unavailable."
  })
  await check("真实交付的Markdown、图片与PDF内容请求及原文件可用", async () => {
    await page.goto(receipts.md.url)
    await page.locator("[data-out-md]").waitFor()
    await page.goto(receipts.image.url)
    await page.locator("[data-delivery-image]").waitFor()
    assert.equal(await page.locator("[data-delivery-image]").evaluate((e) => e.complete && e.naturalWidth > 0), true)
    await shot(page, "real-delivery-image")
    const response = page.waitForResponse((r) => r.url().startsWith(receipts.pdf.content_url) && r.status() === 200)
    await page.goto(receipts.pdf.url)
    await page.locator("[data-delivery-pdf]").waitFor()
    assert.equal((await response).headers()["content-type"], "application/pdf")
    await shot(page, "real-delivery-pdf")
    return "Markdown rendered; PNG decoded; PDF iframe fetched actual application/pdf 200. Native PDF viewer internals were not asserted."
  })
  assert.deepEqual(evidence.pageErrors, [])
} catch (e) {
  evidence.fatal = String(e)
  process.exitCode = 1
  if (page) await shot(page, "fatal").catch(() => {})
  console.error(e)
} finally {
  await browser?.close()
  if (hub.exitCode === null && hub.signalCode === null) hub.kill("SIGTERM")
  const shutdown = setTimeout(() => { if (hub.exitCode === null && hub.signalCode === null) hub.kill("SIGKILL") }, 10_000)
  try { await exited } finally { clearTimeout(shutdown) }
  log.end()
  if (process.env.STEVE_TEST_SHOTS) await writeFile(output + "/live-evidence.json", JSON.stringify(evidence, null, 2))
  // The isolated database and test Bot credentials are never part of delivered evidence.
  await rm(home, { recursive: true, force: true })
}
