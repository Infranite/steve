// 飞书 托管 without a bot (Hub §16): a channel service that names its platform is listed under 飞书 / Lark as an
// installation of its own, “代你处理” through the service; no bot is offered or said to be missing, the Feishu
// app's permissions and console are not shown. The real 接入 page over a fake Hub, 1280 and 390 px.
import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5199"
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

const now = Date.now(), m = 60_000
const health = () => ({ last_ok: now - m, last_error: "", error_at: 0 })
const cap = (area, state) => ({ area, state, missing_scopes: [], suggested_scopes: [], caps: state === "on" ? ["msg.receive"] : [] })
const feishuAdapters = [
  { id: "feishu_app", name: "机器人", identity: "bot", auth: "credential", device: false, capabilities: [{ area: "channel", caps: ["msg.receive"], scopes: [], planned: false }] },
  { id: "feishu_user", name: "你本人", identity: "self", auth: "oauth", device: true, pairing: { with: ["feishu_app"], required: true }, capabilities: [{ area: "channel", caps: ["msg.receive"], scopes: [], planned: false }] },
]
// the proxy: adapter ext_ws, listed under feishu because its service said so
const proxy = { id: "conn_proxy", account_id: "acc_proxy", adapter: "ext_ws", adapter_name: "其他聊天工具", identity: "self", name: "李雷", handle: "acme:id:7001", status: "ok", health: health(), last_sync: now,
  capabilities: [cap("channel", "on"), cap("send_as_you", "off")] }
const integrations = {
  areas: [{ id: "channel", label: "聊天" }, { id: "send_as_you", label: "以你身份发送" }],
  providers: [
    { id: "feishu", name: "飞书 / Lark", status: "supported", areas: [{ area: "channel", state: "supported" }], adapters: feishuAdapters, connections: [proxy] },
    { id: "ext_ws", name: "其他聊天工具", status: "supported", areas: [{ area: "channel", state: "supported" }], adapters: [{ id: "ext_ws", name: "其他聊天工具", identity: "self", auth: "credential", device: false, capabilities: [] }], connections: [] },
  ],
  installations: [{ id: "conn_proxy", provider: "feishu", provider_name: "飞书 / Lark", name: "李雷", status: "ok", missing: 0, identities: [{ kind: "you", connection: proxy }] }],
  callback_urls: [],
}

const run = async ({ width }) => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 }, reducedMotion: "reduce" })
  page.setDefaultTimeout(10000)
  const errors = []; page.on("pageerror", (e) => errors.push(e.message))
  await page.route("**/*", async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith("/api/")) return route.continue()
    const p = u.pathname.slice(4)
    let data
    if (p === "/connections/integrations") data = integrations
    else if (p === "/channels/accounts") data = []
    else if (p === "/vault/status") data = { initialized: true }
    else if (p.endsWith("/registrations/current")) data = null
    else return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) })
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(data) })
  })
  try {
    // the overview: Feishu is connected, its line says who works and nothing about a missing bot
    await page.goto(base + "/test/fixtures/comm-pages.html?view=integrations"); await page.locator('[data-provider-row="feishu"]').waitFor()
    assert.equal(await page.locator('[data-provider-row="feishu"]').getAttribute("data-provider-state"), "connected")
    const line = await page.locator('[data-installation-row="conn_proxy"] [data-installation-line]').innerText()
    assert.match(line, /代你处理/); assert.doesNotMatch(line, /机器人/)
    // the provider page: one installation through the service, no bot to turn on, no Feishu app or console
    await page.goto(base + "/test/fixtures/comm-pages.html?view=provider&id=feishu"); await page.locator('[data-installation="conn_proxy"]').waitFor()
    const card = page.locator('[data-installation="conn_proxy"]')
    assert.match(await card.innerText(), /通过渠道服务托管，不需要机器人/)
    assert.match(await card.locator('[data-identity="you"]').innerText(), /通过你运行的渠道服务/)
    assert.equal(await card.locator('[data-identity="steve"]').count(), 0, "no bot section in a service's installation")
    assert.equal(await card.locator("[data-who-works]").count(), 0)
    assert.equal(await card.getByText("开发者后台").count(), 0)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
    if (shots) await page.screenshot({ path: `${shots}/feishu-proxy-${width}.png`, fullPage: true })
    assert.deepEqual(errors, [])
  } catch (e) { console.error(await page.locator("body").innerText(), errors); throw e } finally { await page.close() }
}
try { for (const width of [1280, 390]) await run({ width }) } finally { await browser.close() }
console.log(JSON.stringify({ passed: true, scenarios: ["proxy listed under feishu", "no missing bot", "no bot section", "no Feishu app details", "1280 and 390 px"] }))
