import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 390, height: 844 } }), errors = []
page.on("pageerror", e => errors.push(e.message))
const message = { id: "cm_fixture", space_id: "space", direction: "in", sender: { name: "同事", party_id: null }, persona_id: null, on_behalf: "none", text: "API摘要不重复展示", status: "received", work_item_id: null, decision_id: null, created: 100, sent: null, purged: null, basis: null,
  attachments: [{ type: "card", schema: 2, title: "两处入口的同一张卡片", blocks: [{ t: "markdown", text: "**共同正文**" }] }, { type: "file", name: "资料.pdf", url: "/api/chan-messages/cm_fixture/resources/1" }] }
const badge = { space: "space", space_id: "space", name: "项目群", kind: "group", dm: false, system: "feishu", provider: "飞书", has_external: false, member_count: 3, mode: "bot" }
let purged = false
await page.route("**/*", route => {
  const u = new URL(route.request().url()); if (u.origin !== base) return route.abort()
  if (!u.pathname.startsWith("/api/")) return route.continue()
  let data
  if (u.pathname === "/api/conversations") data = { conversations: [] }
  else if (u.pathname === "/api/conversations/space/messages") data = { conversation: badge, messages: [{ id: "m1", author: "me", text: "旧的文字不应泄漏", item_id: null, meta: { chan: { message: "cm_fixture", sender: "同事" } }, created: 100, channel_message: purged ? { ...message, text: null, attachments: [], purged: 200 } : message }] }
  else if (u.pathname === "/api/conversations/other/messages") return route.fulfill({ status: 404, contentType: "application/json", body: '{"error":"没有这个对话"}' })
  else if (u.pathname === "/api/spaces/space") data = { id: "space", connection_id: "connection", kind: "group", name: "项目群", member_count: 3, has_external: false, status: "active", persona: null, duty: "", rules: [], effective_rules: [], flow_id: null, muted: true, collab: false, memory_scope: "space", disclosure: "never", item_defaults: { tags: [], project_id: null, bot_id: null }, configured: true, teams: [], revision: 1, recent: [message] }
  else if (["/api/connections", "/api/personas"].includes(u.pathname)) data = []
  else if (u.pathname === "/api/connections/integrations") data = { areas: [], providers: [], callback_urls: [] }
  else throw new Error(`unexpected API ${u.pathname}`)
  return route.fulfill({ contentType: "application/json", body: JSON.stringify(data) })
})
try {
  await page.goto(base + "/test/fixtures/message-surfaces.html?recent")
  await page.getByText("两处入口的同一张卡片", { exact: true }).waitFor()
  assert.equal(await page.getByRole("button", { name: "下载：资料.pdf" }).count(), 1)
  assert.equal(await page.getByText("API摘要不重复展示", { exact: true }).count(), 0)
  await page.goto(base + "/test/fixtures/message-surfaces.html")
  await page.getByText("两处入口的同一张卡片", { exact: true }).waitFor()
  assert.equal(await page.getByRole("button", { name: "下载：资料.pdf" }).count(), 1)
  assert.equal(await page.getByText("旧的文字不应泄漏", { exact: true }).count(), 0)
  await page.evaluate(() => window.dispatchEvent(new Event("fixture-switch")))
  await page.locator('[data-conv-missing]').waitFor()
  assert.equal(await page.getByText("两处入口的同一张卡片", { exact: true }).count(), 0)
  purged = true; await page.goto(base + "/test/fixtures/message-surfaces.html")
  await page.getByText("原文已过保留期限", { exact: true }).waitFor()
  assert.equal(await page.getByText("旧的文字不应泄漏", { exact: true }).count(), 0)
  assert.equal(await page.getByRole("button", { name: "下载：资料.pdf" }).count(), 0)
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["actual SpaceView", "actual ChatView", "same card and attachment", "conversation switch clears content", "purged source hides old conversation text"] }))
} catch (e) { console.error(await page.locator("body").innerText(), errors); throw e } finally { await browser.close() }
