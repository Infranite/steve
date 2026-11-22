/* Same-ID draft updates through the real store refresh and API helpers in Chrome. All API traffic is intercepted;
 * the stateful fixture models edits, approvals and undo, and never sends to a Hub or messaging provider. */
import assert from "node:assert/strict"
import { mkdirSync, writeFileSync } from "node:fs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })
let assertions = 0
const evidence = []
const eq = (a, b, why) => { assert.deepEqual(a, b, why); assertions++ }
const ok = (v, why) => { assert.ok(v, why); assertions++ }
const initial = "我是值班助手，是负责答疑的 AI 助手。\n你可以直接告诉我需要什么帮助。"
const updated = "另一端修改并撤销批准后保留的新正文。"
const newer = "刚刚同步的第二版正式稿。\n请以此稿为准。"
const local = "我尚未提交的修改。\n需要保留并对照。"

function scenario(name) {
  let decision = { id: "dc_reply", ws: "ws_fixture", kind: "approve", subject_type: "chan_message", subject_id: "cm_reply", item_id: null, session_id: null, question: "审批渠道回复", draft: initial, options: ["yes", "no", "to_item"], status: "open", result: null, requested_by: "steve", created: 1, resolved: null, undo_until: null, space_id: null, tier: "decide", folded: null }
  let rev = 1
  let failNext = false
  const pages = [], errors = [], writes = [], events = []
  const record = { scenario: name, writes, events }
  evidence.push(record)
  return {
    writes,
    change(draft) { decision = { ...decision, draft }; rev++; events.push({ action: "fixture-updates-formal-draft", id: decision.id, draft }) },
    failNext() { failNext = true },
    async open(kind = "draft", { width = 1280, lang = "zh-CN" } = {}) {
      const page = await browser.newPage({ viewport: { width, height: 1000 }, locale: lang, reducedMotion: "reduce" })
      const client = pages.length + 1
      pages.push(page)
      page.on("pageerror", (e) => errors.push(e.message))
      await page.addInitScript((lang) => localStorage.setItem("steve-lang", lang), lang)
      await page.route("**/*", async (route) => {
        const request = route.request(), u = new URL(request.url())
        if (u.origin !== base) return route.abort()
        if (!u.pathname.startsWith("/api/")) return route.continue()
        if (request.method() === "POST" && u.pathname.startsWith("/api/decisions/")) {
          const body = request.postData() ? request.postDataJSON() : null
          const call = { client, path: u.pathname, body, status: 200 }
          writes.push(call)
          if (failNext) {
            failNext = false; call.status = 503
            return route.fulfill({ status: 503, json: { error: "fixture: submission unavailable" } })
          }
          if (u.pathname === "/api/decisions/dc_reply/undo") {
            decision = { ...decision, status: "open", result: null, resolved: null }
          } else {
            eq(u.pathname, "/api/decisions/dc_reply", "every approval targets the same decision ID")
            decision = { ...decision, draft: body.text ?? decision.draft, status: "resolved", result: { choice: body.choice, send_at: 2000 }, resolved: Date.now() }
          }
          return route.fulfill({ headers: { "x-steve-rev": String(++rev) }, json: decision })
        }
        if (u.pathname === "/api/snapshot") {
          events.push({ action: "snapshot-read", client, id: decision.id, status: decision.status, draft: decision.draft })
          return route.fulfill({ json: { rev, cursor: String(rev), views: { decision: decision.status === "open" ? [decision] : [] } } })
        }
        if (u.pathname === "/api/views") return route.fulfill({ json: { rev, views: [decision], missing: [] } })
        if (u.pathname === "/api/views/page") return route.fulfill({ json: { rev, views: [], next: null } })
        if (u.pathname.endsWith("/context")) return route.fulfill({ json: { space: { id: "sp_product", name: "产品讨论群", kind: "group", external: true }, as_you: false, persona: "值班助手", text: decision.draft, reply_to: { id: "cm_in", sender: "陈同学", text: "你是谁啊？", at: 1 }, before: [] } })
        return route.fulfill({ status: 404, json: { error: "fixture: unavailable" } })
      })
      await page.goto(`${base}/test/fixtures/channel-reply.html?case=${kind}`)
      await page.locator("[data-channel-reply]").first().waitFor()
      await page.locator("[data-channel-reply]").first().evaluate((el) => el.setAttribute("data-preserved-view", "mounted"))
      return page
    },
    async shot(page, suffix) { if (shots) await page.screenshot({ path: `${shots}/${name}-${suffix}.png`, fullPage: true }) },
    async close() {
      eq(errors, [], "no browser runtime errors")
      for (const p of pages) {
        ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
        await p.close()
      }
    },
  }
}
async function refresh(page) {
  await Promise.all([page.waitForResponse((r) => new URL(r.url()).pathname === "/api/snapshot"), page.locator("[data-fixture-fresh]").click()])
}
async function shown(page, text) {
  await page.waitForFunction((text) => document.querySelector("[data-reply-draft]")?.textContent === text, text)
  eq(await page.locator("[data-channel-reply]").first().getAttribute("data-preserved-view"), "mounted", "same-ID update keeps the mounted card")
}
async function sent(page) { await page.waitForFunction(() => !document.querySelector("[data-channel-reply]")) }

try {
  const idle = scenario("same-id-refresh")
  const p = await idle.open()
  idle.change(updated)
  await refresh(p)
  await shown(p, updated)
  idle.change(newer)
  await refresh(p)
  await shown(p, newer)
  await p.locator("[data-reply-edit]").click()
  eq(await p.locator("[data-reply-editor]").inputValue(), newer, "entering edit reads the newest formal draft")
  await p.locator("[data-reply-editor]").fill(local)
  await p.locator("[data-reply-cancel]").click()
  idle.change(updated)
  await refresh(p)
  await shown(p, updated)
  await p.locator("[data-reply-edit]").click()
  eq(await p.locator("[data-reply-editor]").inputValue(), updated, "re-entering edit never restores a cancelled or initial draft")
  await idle.shot(p, "editor")
  await p.locator("[data-reply-send]").evaluate((b) => { b.click(); b.click() })
  await sent(p)
  eq(idle.writes.map((w) => w.body), [{ choice: "yes", text: updated }], "double click posts exactly the newest reviewed text once")
  await idle.close()

  const peer = scenario("other-client-approval-undo")
  const first = await peer.open(), second = await peer.open()
  await second.locator("[data-reply-edit]").click()
  await second.locator("[data-reply-editor]").fill(updated)
  await second.locator("[data-reply-send]").click()
  await sent(second)
  await Promise.all([second.waitForResponse((r) => new URL(r.url()).pathname === "/api/snapshot"), second.locator("[data-fixture-undo]").click()])
  await second.locator("[data-reply-draft]").waitFor()
  eq(await second.locator("[data-reply-draft]").textContent(), updated, "undo retains the modified formal draft in the fixture contract")
  await refresh(first)
  await shown(first, updated)
  await first.locator("[data-reply-edit]").click()
  eq(await first.locator("[data-reply-editor]").inputValue(), updated, "another client's approved then undone text seeds the editor")
  await peer.shot(first, "refreshed-editor")
  await first.locator("[data-reply-send]").click()
  await sent(first)
  eq(peer.writes.map(({ client, path, body }) => ({ client, path, body })), [
    { client: 2, path: "/api/decisions/dc_reply", body: { choice: "yes", text: updated } },
    { client: 2, path: "/api/decisions/dc_reply/undo", body: {} },
    { client: 1, path: "/api/decisions/dc_reply", body: { choice: "yes", text: updated } },
  ], "both browser clients and the undo helper post the intended payloads")
  await peer.close()

  for (const [resolution, width, lang] of [["use-latest", 1280, "zh-CN"], ["keep-edits", 390, "zh-CN"], ["cancel", 390, "en"]]) {
    const x = scenario(`editing-conflict-${resolution}`)
    const page = await x.open("draft", { width, lang })
    await page.locator("[data-reply-edit]").click()
    await page.locator("[data-reply-editor]").fill(local)
    x.change(updated)
    await refresh(page)
    await page.locator("[data-reply-conflict]").waitFor()
    eq(await page.locator("[data-reply-editor]").inputValue(), local, "formal updates preserve unsaved input")
    eq(await page.locator("[data-reply-latest]").textContent(), updated, "conflict shows the current formal text for comparison")
    ok(await page.locator("[data-reply-send]").isDisabled(), "conflict blocks approval")
    await page.locator("[data-reply-send]").evaluate((b) => b.click())
    eq(x.writes.length, 0, "no stale text is posted while unresolved")
    await x.shot(page, "blocked")
    if (resolution === "cancel") {
      await page.locator("[data-reply-cancel]").click()
      await shown(page, updated)
      eq(await page.locator("[data-reply-conflict]").count(), 0, "cancel returns to the latest formal draft")
      await page.locator("[data-reply-send]").click()
      await sent(page)
      eq(x.writes[0].body, { choice: "yes" }, "approval after cancel uses the formal draft without stale edits")
    } else {
      await page.locator(`[data-reply-${resolution}]`).click()
      eq(await page.locator("[data-reply-conflict]").count(), 0, "the chosen resolution clears conflict")
      const expected = resolution === "use-latest" ? updated : local
      eq(await page.locator("[data-reply-editor]").inputValue(), expected)
      // A second formal update needs a new explicit choice, even after the first was acknowledged.
      x.change(newer)
      await refresh(page)
      await page.locator("[data-reply-conflict]").waitFor()
      eq(await page.locator("[data-reply-latest]").textContent(), newer)
      ok(await page.locator("[data-reply-send]").isDisabled(), "a later update re-blocks approval")
      await page.locator(`[data-reply-${resolution}]`).click()
      const final = resolution === "use-latest" ? newer : local
      x.failNext()
      await page.locator("[data-reply-send]").click()
      await page.locator("[data-reply-error]").waitFor()
      eq(await page.locator("[data-reply-editor]").inputValue(), final, "failed submit retains the explicitly reviewed version")
      eq(await page.locator("[data-reply-result]").count(), 0, "failure never claims a send")
      await page.locator("[data-reply-send]").evaluate((b) => { b.click(); b.click() })
      await sent(page)
      eq(x.writes.map((w) => w.body), [{ choice: "yes", text: final }, { choice: "yes", text: final }], "failed attempt and single retry both use the explicitly chosen text")
      eq(x.writes.map((w) => w.status), [503, 200])
    }
    await x.close()
  }

  const multi = scenario("multiple-views")
  const page = await multi.open("duplicate")
  const cards = page.locator("[data-channel-reply]")
  await cards.first().locator("[data-reply-edit]").click()
  await cards.first().locator("[data-reply-editor]").fill(local)
  multi.change(updated)
  await refresh(page)
  await cards.first().locator("[data-reply-conflict]").waitFor()
  eq(await cards.first().locator("[data-reply-editor]").inputValue(), local)
  eq(await cards.last().locator("[data-reply-draft]").textContent(), updated, "the idle view displays the update")
  await cards.last().locator("[data-reply-edit]").click()
  eq(await cards.last().locator("[data-reply-editor]").inputValue(), updated, "the idle view enters editing with the update")
  ok(await cards.first().locator("[data-reply-send]").isDisabled(), "another view cannot silently use old input")
  await multi.shot(page, "independent-editors")
  await cards.last().locator("[data-reply-send]").evaluate((b) => { b.click(); b.click() })
  await sent(page)
  eq(multi.writes.map((w) => w.body), [{ choice: "yes", text: updated }], "only the current reviewed view posts")
  await multi.close()

  for (const resolution of ["use-latest", "keep-edits"]) {
    const x = scenario(`empty-to-formal-${resolution}`)
    const p = await x.open("empty")
    await p.locator("[data-reply-editor]").fill(local)
    x.change(updated)
    await refresh(p)
    await p.locator("[data-reply-conflict]").waitFor()
    eq(await p.locator("[data-reply-editor]").inputValue(), local, "a draft arriving during manual composition keeps input")
    ok(await p.locator("[data-reply-send]").isDisabled())
    await p.locator(`[data-reply-${resolution}]`).click()
    await p.locator("[data-reply-send]").click()
    await sent(p)
    eq(x.writes[0].body, { choice: "yes", text: resolution === "use-latest" ? updated : local })
    await x.close()
  }
} finally {
  if (shots) writeFileSync(`${shots}/refresh-receipts.json`, JSON.stringify({ assertions, note: "Chrome with intercepted stateful fixture API only. No Hub or provider send; undo uses the fixture control and actual api.undo helper.", evidence }, null, 2))
  await browser.close()
}
console.log(`channel-reply-refresh: ${assertions} assertions passed (stateful fixture API; no external send)`)
