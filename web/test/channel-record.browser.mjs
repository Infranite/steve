/* A channel conversation's full record in 对话 (#243), on the real chat page against a fixture-only API: the channel
 * switcher, every message of the chat woven in between the turns with what Steve did with it (a failed trigger word with
 * its reason and the way to your own conversation that answered it, a skip, a handoff, a card waiting), a turn's message shown once, earlier pages, the read marker, and the
 * switcher opening an account's conversations. No request reaches a Hub. */
import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })

const now = Date.now(), min = 60_000
const badge = { space: "chan:sp_ops", space_id: "sp_ops", name: "运维群", provider: "飞书", dm: false, kind: "group", system: "feishu", member_count: 6, mode: "bot", has_external: false, connection: "con_bot", open_url: "https://applink.feishu.cn/client/chat/open?openChatId=oc_ops" }
const chan = (id, created, direction, sender, text, more = {}) => ({ id, space_id: "sp_ops", direction, sender: { name: sender, party_id: null }, persona_id: null, on_behalf: "none", text, attachments: [], mentions: [], status: direction === "in" ? "received" : "sent", thread_key: null, reply_to: null, event_id: null, work_item_id: null, decision_id: null, created, sent: null, purged: null, basis: null, history: null, open_url: badge.open_url, turn: null, ...more })
const h = (state, more = {}) => ({ state, label: "", reason: "", error: null, item: null, decision: null, ...more })
const page1 = [
  chan("cm_lunch", now - 50 * min, "in", "韩梅梅", "我先去吃饭", { handling: h("recorded", { label: "没有匹配的规则，只记录" }) }),
  chan("cm_in1", now - 40 * min, "in", "李雷", "部署挂了怎么办", { turn: "t1", handling: h("answered") }),
  chan("cm_out1", now - 39 * min, "out", "值班小运", "先看部署日志，再按回滚手册处理。", { turn: "s1", out: { status: "sent", decision: null, error: null } }),
  chan("cm_ai", now - 20 * min, "in", "你", "/ai 看下我现在的磁盘情况", { handling: h("failed", { error: "会话没有启动：模型服务暂时不可用", main_turn: "tm_ai" }) }),
  chan("cm_item", now - 10 * min, "in", "李雷", "帮忙把回滚手册更新一下", { handling: h("item", { item: "ST-12" }) }),
  chan("cm_wait", now - 5 * min, "in", "韩梅梅", "明天的发布还按计划吗？", { handling: h("waiting", { decision: "dec_1" }) }),
  chan("cm_late", now - 2 * min, "out", "值班小运", "收到，稍后回复。", { out: { status: "failed", decision: "dec_2", error: "发到「运维群」的消息没有发出去：飞书暂时不可用" } }),
]
const page0 = [chan("cm_old", now - 26 * 60 * min, "in", "李雷", "上周的值班表发一下", { handling: h("skipped", { label: "不需要回复", reason: "起草器：已经有人回答了" }) })]
const messages = [
  { id: "t1", author: "me", text: "部署挂了怎么办", created: now - 40 * min + 1, space: "chan:sp_ops", meta: { chan: { message: "cm_in1", sender: "李雷", space: "sp_ops" } }, reaction: { state: "done", emoji: "DONE", as_user: false, error: null, updated: now }, channel_message: page1[1] },
  { id: "s1", author: "steve", text: "先看部署日志，再按回滚手册处理。", created: now - 39 * min - 1, space: "chan:sp_ops", meta: { origin: "t1", chan_out: { message: "cm_out1", status: "sent", decision: null } } },
]
const channels = [
  { connection: "con_bot", name: "值班小运", provider: "飞书", mode: "bot", status: "ok", conversations: 3, unread: 2 },
  { connection: "con_me", name: "我的飞书", provider: "飞书", mode: "self", status: "ok", conversations: 12, unread: 0 },
]
const row = (space, name, more = {}) => ({ ...badge, space: "chan:" + space, space_id: space, name, count: 0, last: now - min, last_text: "最近一条", last_author: "me", closed: false, unread: 0, ...more })

let checks = 0
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 900 }, locale: "zh-CN", reducedMotion: "reduce" })
    page.setDefaultTimeout(12000)
    const errors = [], reads = [], lists = []
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", async (route) => {
      const req = route.request(), u = new URL(req.url())
      if (u.origin !== base) return route.abort()
      if (!u.pathname.startsWith("/api/")) return route.continue()
      const p = decodeURIComponent(u.pathname)
      if (p === "/api/conversations/channels") return route.fulfill({ json: { channels } })
      if (p === "/api/conversations/chan:sp_ops/messages") return route.fulfill({ json: { conversation: badge, messages } })
      if (p === "/api/conversations/chan:sp_ops/record") {
        const older = u.searchParams.get("before_id") === "cm_lunch"
        return route.fulfill({ json: older ? { records: page0, has_more: false, next: null } : { records: page1, has_more: true, next: { before: page1[0].created, before_id: "cm_lunch" } } })
      }
      if (p === "/api/conversations/chan:sp_ops/read" && req.method() === "POST") { reads.push(req.postDataJSON().at); return route.fulfill({ json: { ok: true } }) }
      if (p === "/api/conversations") {
        lists.push(u.searchParams.get("connection") ?? "")
        const mine = u.searchParams.get("connection") === "con_me"
        return route.fulfill({ json: { conversations: mine ? [row("sp_dm", "李雷", { dm: true, kind: "dm", mode: "self", connection: "con_me" })] : [row("sp_ops", "运维群", { unread: 2 }), row("sp_rel", "发布群", { unread: 120 })], total: mine ? 1 : 2, has_more: false, next_offset: null } })
      }
      if (req.method() !== "GET") return route.fulfill({ status: 405, json: { error: "fixture forbids writes" } })
      return route.fulfill({ status: 404, json: { error: "fixture" } })
    })
    const fits = async () => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no sideways scroll at ${width}`)
    const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/channel-record-${name}-${width}.png` }) }
    try {
      await page.goto(base + "/test/fixtures/chat-layout.html?conv=chan:sp_ops")
      // the switcher: Steve, then each account; this conversation's account is the one picked, with its new conversations
      const sw = page.locator("[data-channel-switcher]")
      await sw.waitFor()
      for (const name of ["Steve", "值班小运", "我的飞书"]) await sw.getByText(name, { exact: true }).waitFor()
      assert.equal(await sw.locator('[data-channel="con_bot"]').getAttribute("aria-checked"), "true")
      assert.match(await sw.locator('[data-channel="con_bot"]').innerText(), /2/)
      // every message of the chat, by time; a turn's message once
      await page.locator('[data-record="in"][data-msg="cm_ai"]').waitFor()
      assert.equal(await page.getByText("部署挂了怎么办", { exact: true }).count(), 1, "the turn shows its chat message once")
      assert.equal(await page.getByText("先看部署日志，再按回滚手册处理。", { exact: true }).count(), 1, "Steve's answer once")
      const order = await page.locator("[data-chat-messages] [data-msg]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-msg")))
      assert.deepEqual(order, ["cm_lunch", "t1", "s1", "cm_ai", "cm_item", "cm_wait", "cm_late"])
      // what Steve did: the failed trigger word says why, a handoff links its item, a card waits for you
      const ai = page.locator('[data-msg="cm_ai"] [data-handling="failed"]')
      assert.match(await ai.innerText(), /失败/)
      assert.match(await ai.innerText(), /模型服务暂时不可用/)
      assert.match(await ai.locator('[data-handling-main="tm_ai"]').innerText(), /到 Steve 里看/)
      assert.match(await page.locator('[data-msg="cm_lunch"] [data-handling="recorded"]').innerText(), /只记录.*没有匹配的规则/s)
      await page.locator('[data-msg="cm_wait"] [data-handling-decision="dec_1"]').waitFor()
      assert.match(await page.locator('[data-msg="cm_late"] [data-chan-out="failed"]').innerText(), /没有发出去.*飞书暂时不可用/s)
      assert.equal(await page.locator('[data-msg="t1"] [data-handling]').count(), 0, "the reaction already says it was answered")
      // read up to the newest message shown
      await page.waitForFunction(() => true)
      for (let i = 0; i < 50 && !reads.length; i++) await page.waitForTimeout(20)
      assert.deepEqual(reads, [page1.at(-1).created])
      await fits(); await shot("record")
      // earlier messages, a page at a time
      await page.locator("[data-record-older] button").click()
      await page.locator('[data-msg="cm_old"] [data-handling="skipped"]').waitFor()
      assert.match(await page.locator('[data-msg="cm_old"] [data-handling="skipped"]').innerText(), /已经有人回答了/)
      assert.equal(await page.locator("[data-record-older]").count(), 0)
      await page.locator('[data-msg="cm_item"] [data-handling-item="ST-12"]').click()
      await page.waitForFunction(() => location.hash === "#/items/ST-12")
      // the switcher opens an account's conversations; the list has the account picked, the time and what is new
      await page.locator('[data-channel="con_bot"]').click()
      await page.locator("[data-conversation-manager]").waitFor()
      for (let i = 0; i < 50 && lists.at(-1) !== "con_bot"; i++) await page.waitForTimeout(20)
      assert.equal(lists.at(-1), "con_bot", "the account already picked opens its conversations again")
      await page.keyboard.press("Escape")
      await page.locator("[data-conversation-manager]").waitFor({ state: "hidden" })
      await page.locator('[data-channel="con_me"]').click()
      await page.locator("[data-conversation-manager]").waitFor()
      await page.locator('[data-conversation-row="chan:sp_dm"]').waitFor()
      assert.equal(lists.at(-1), "con_me")
      await page.locator("[data-conversation-manager]").getByRole("radio", { name: "全部渠道", exact: true }).click()
      await page.locator('[data-conversation-row="chan:sp_rel"] [data-conversation-unread]').waitFor()
      assert.equal(lists.at(-1), "")
      assert.equal(await page.locator('[data-conversation-row="chan:sp_rel"] [data-conversation-unread]').innerText(), "99+")
      await page.locator('[data-conversation-row="chan:sp_ops"] [data-conversation-last]').waitFor()
      await fits(); await shot("list")
      assert.deepEqual(errors, [])
      checks++
    } catch (e) { if (shots) await page.screenshot({ path: `${shots}/channel-record-failure-${width}.png`, fullPage: true }); console.error(errors); throw e } finally { await page.close() }
  }
  console.log(JSON.stringify({ passed: true, widths: [1280, 390], checks, source: "fixture-only; no real Hub" }))
} finally { await browser.close() }
