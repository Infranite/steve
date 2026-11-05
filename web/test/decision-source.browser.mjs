import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
/* ST-29: a decision in an IM conversation is answered in that conversation, once, and stays closed after a reload;
 * a follow-up question in your own conversation is answered once. Mocked API, no Hub. */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })
const badge = { space: "chan:sp1", space_id: "sp1", name: "地狱焚诀 Agent 交流群", kind: "group", dm: false, system: "feishu", provider: "飞书", has_external: false, member_count: 0, mode: "bot" }
let out = "draft", decided = [], said = [], gate = null, late = false
const messages = () => [
  { id: "m1", author: "me", text: "@李鹏翔 明天", item_id: null, meta: { chan: { message: "cm_in", sender: "李昊元" } }, created: Date.now() - 60000, reaction: out === "draft" ? { state: "ack", emoji: null, as_user: false, error: null, updated: 1 } : { state: "cleared", emoji: null, as_user: false, error: null, updated: 2 }, channel_message: null },
  { id: "m2", author: "steve", text: "这条消息看起来是发给李鹏翔的，而且没写完，你是想让我帮忙处理吗？", item_id: null, created: Date.now() - 50000,
    meta: { origin: "m1", ask: { options: ["这条是发给李鹏翔的，我不用处理", "想让我帮你处理什么？请补充完整"] }, chan_out: { message: "cm_out", status: out, decision: out === "draft" ? "dc1" : null } } },
]
try {
  for (const [width, theme] of [[1280, "dark"], [390, "light"]]) {
    out = "draft"; decided = []; said = []
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 844 } }), errors = []
    page.on("pageerror", e => errors.push(e.message))
    await page.route("**/*", async route => {
      const u = new URL(route.request().url()); if (u.origin !== base) return route.abort()
      if (!u.pathname.startsWith("/api/")) return route.continue()
      const m = route.request().method()
      if (u.pathname === "/api/conversations") return route.fulfill({ json: { conversations: [] } })
      if (u.pathname === "/api/conversations/chan%3Asp1/messages" || u.pathname === "/api/conversations/chan:sp1/messages") return route.fulfill({ json: { conversation: badge, messages: messages() } })
      if (u.pathname === "/api/decisions/dc1" && m === "POST") {
        decided.push(route.request().postDataJSON().choice)
        if (gate) await gate
        if (late) return route.fulfill({ json: { id: "dc1", status: "resolved", result: { choice: "no" } } })
        if (out !== "draft") return route.fulfill({ status: 409, json: { error: "这个决定已经处理过了" } })
        out = "discarded"; late = false
        return route.fulfill({ json: { id: "dc1", status: "resolved", result: { choice: "no" } } })
      }
      if (u.pathname === "/api/thread" && m === "POST") { said.push(route.request().postDataJSON().text); return route.fulfill({ json: { id: "u" + said.length } }) }
      if (u.pathname === "/api/state") return route.fulfill({ status: 503, json: { error: "fixture" } })
      throw new Error(`unexpected API ${m} ${u.pathname}`)
    })
    const q = theme === "dark" ? "?dark" : ""
    await page.goto(base + "/test/fixtures/decision-source.html" + q)
    await page.locator("[data-chan-out-decision='dc1']").waitFor()
    // the question's options are what the chat can reply, not buttons that speak in your conversation
    assert.equal(await page.locator("[data-ask-opt]").count(), 0)
    await page.locator("[data-ask-readonly]").getByText("这条是发给李鹏翔的，我不用处理", { exact: false }).waitFor()
    // sending is approved on the 待处理 card, which shows the text that would go out and any warning
    assert.equal(await page.locator("[data-chan-out-send]").count(), 0)
    await page.locator("[data-chan-out-review]").waitFor()
    if (shots) await page.screenshot({ path: `${shots}/conv-draft-${width}-${theme}.png` })
    // 不发出, clicked twice while the first is on its way: one answer
    let unblock; gate = new Promise((r) => { unblock = r })
    const discard = page.locator("[data-chan-out-discard]")
    await discard.click()
    await page.waitForFunction(() => document.querySelector("[data-chan-out-discard]")?.disabled === true)
    await discard.click({ force: true }).catch(() => {})
    unblock(); gate = null
    await page.locator("[data-chan-out='discarded']").waitFor()
    assert.equal(await page.locator("[data-chan-out-decision]").count(), 0)
    assert.deepEqual(decided, ["no"], "answered once")
    assert.deepEqual(said, [], "nothing went to your own conversation")
    if (shots) await page.screenshot({ path: `${shots}/conv-closed-${width}-${theme}.png` })
    // a reload reads it from the Hub: still closed, no buttons
    await page.reload()
    await page.locator("[data-chan-out='discarded']").waitFor()
    assert.equal(await page.locator("[data-chan-out-decision]").count(), 0)
    assert.equal(await page.getByText("你没有发出", { exact: true }).count(), 1)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no sideways overflow")
    // a click answered late does not cover what the Hub says now
    out = "draft"; decided = []
    await page.reload()
    let unblock2; gate = new Promise((r) => { unblock2 = r }); late = true
    await page.locator("[data-chan-out-discard]").click()
    out = "sent"
    await page.evaluate(() => window.dispatchEvent(new Event("fixture-reload")))
    await page.locator("[data-chan-out='sent']").waitFor()
    unblock2(); gate = null
    await page.waitForFunction(() => document.querySelector("[data-chan-out]") && !document.querySelector("[data-chan-out-discard]"))
    await page.waitForTimeout(300)
    assert.equal(await page.locator("[data-chan-out='sent']").count(), 1, "the Hub's 已发出 stays")
    out = "discarded"; late = false

    // your own conversation: one click answers once, the options are gone afterwards
    await page.goto(base + "/test/fixtures/decision-source.html?main" + (q ? "&dark" : ""))
    const opt = page.locator("[data-ask-opt='这条是发给李鹏翔的，我不用处理']")
    await opt.waitFor()
    await opt.dblclick()
    await page.waitForFunction(() => !document.querySelector("[data-ask-opt]"))
    assert.deepEqual(said, ["这条是发给李鹏翔的，我不用处理"], "sent once")
    if (shots) await page.screenshot({ path: `${shots}/main-answered-${width}-${theme}.png` })
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("decision-source: ok")
} finally { await browser.close() }
