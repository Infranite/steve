/* Main conversation history in the browser: one compact bar; history search and topics;
 * a jump lands on the message (loading what lies between) and the way back returns to the same place; older messages
 * come in above without moving what you read; the reading position and the draft survive a reload. */
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { created, older, text } from "./fixtures/chat-history-data.ts"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const hub = Array.from({ length: older }, (_, i) => ({ id: "o" + i, author: i % 2 ? "steve" : "me", text: text(i), item_id: i === 51 ? "ST-7" : null, meta: null, created: created(i) }))
const errors = []
const near = (a, b, d = 4) => a !== null && Math.abs(a - b) <= d
try {
  for (const width of [1440, 390]) for (const theme of ["dark", "light"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 900 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    const calls = []
    let holdOlder
    await page.route("**/api/**", async route => {
      const u = new URL(route.request().url())
      if (!u.pathname.startsWith("/api/")) return route.continue()
      calls.push(u.pathname + u.search)
      if (u.pathname === "/api/views/page" && u.searchParams.get("kind") === "message") {
        if (holdOlder) await holdOlder
        const before = Number(u.searchParams.get("before")?.split(":")[0] ?? Infinity)
        const all = hub.filter(m => m.created < before).reverse(), views = all.slice(0, 100)
        const last = views.at(-1)
        return route.fulfill({ json: { rev: 0, views, next: all.length > views.length ? `${last.created}:${last.id}` : null } })
      }
      if (u.pathname === "/api/thread/search") {
        const q = (u.searchParams.get("q") ?? "").toLowerCase()
        return route.fulfill({ json: { messages: hub.filter(m => m.text.toLowerCase().includes(q) || m.item_id?.toLowerCase() === q).reverse() } })
      }
      if (u.pathname === "/api/conversations") return route.fulfill({ json: { conversations: [], has_more: false, next_offset: null, total: 0 } })
      return route.fulfill({ status: 404, json: {} })
    })
    await page.goto(base + "/test/fixtures/chat-history.html")
    await page.evaluate(() => localStorage.clear())
    await page.reload()
    const scroller = page.locator("[data-chat-scroll]")
    await page.locator('[data-msg="w499"]').waitFor()
    await page.waitForTimeout(300)
    const atBottom = () => scroller.evaluate(el => el.scrollHeight - el.scrollTop - el.clientHeight < 2)
    assert.ok(await atBottom())
    /** the message at the top of the list and its offset, as the reader sees it */
    const anchor = () => scroller.evaluate(el => { const t = el.getBoundingClientRect().top; for (const x of el.querySelectorAll("[data-msg]")) { const r = x.getBoundingClientRect(); if (r.bottom > t + 1) return { id: x.dataset.msg, off: r.top - t } } })
    const offsetOf = id => scroller.evaluate((el, id) => {
      const row = el.querySelector(`[data-msg="${id}"]`)
      return row ? row.getBoundingClientRect().top - el.getBoundingClientRect().top : null
    }, id)
    const waitForAnchor = async ({ id, off }, tolerance = 4) => {
      // A virtual row can briefly align before a later layout pass changes its position.
      const aligned = await scroller.evaluate(async (el, { id, off, tolerance }) => {
        const deadline = performance.now() + 3000
        let since
        while (performance.now() < deadline) {
          const now = await new Promise(requestAnimationFrame)
          const row = el.querySelector(`[data-msg="${id}"]`)
          if (row && Math.abs(row.getBoundingClientRect().top - el.getBoundingClientRect().top - off) <= tolerance) {
            since ??= now
            if (now - since >= 150) return true
          } else since = undefined
        }
        return false
      }, { id, off, tolerance })
      assert.ok(aligned, `${id} did not settle at offset ${off} (at ${await offsetOf(id)})`)
    }

    // A8: one bar — who, Steve's own state, 历史 and 事项; no “你和 Steve / 会话列表” row, no “在跟 N 件事”.
    const header = page.locator("[data-chat-header]")
    assert.ok((await header.boundingBox()).height <= 52)
    assert.equal(await page.locator("[data-conv-switcher]").count(), 0)
    assert.equal(await page.getByText("你和 Steve").count(), 0)
    assert.equal(await page.getByText("会话列表").count(), 0)
    assert.ok(!(await page.locator("[data-hero-label]").textContent()).includes("在跟"))
    await page.locator("[data-history-toggle]").waitFor()
    await page.locator("[data-rail-toggle]").waitFor()
    if (shots) await page.screenshot({ path: `${shots}/main-${width}-${theme}.png`, animations: "disabled" })

    // A4: reading up, a new message does not move the view; the pill counts it.
    await page.mouse.move(width / 2, 400)
    await page.mouse.wheel(0, -1500)
    await page.waitForTimeout(300)
    const reading = await anchor()
    await page.evaluate(() => window.dispatchEvent(new Event("fixture-reply")))
    await page.locator("[data-new-messages]").waitFor()
    await page.waitForTimeout(200)
    const afterReply = await offsetOf(reading.id)
    assert.ok(near(afterReply, reading.off), `a new message moved ${reading.id}: ${reading.off} → ${afterReply}`)
    assert.equal((await page.locator("[data-new-messages]").textContent()).trim(), "1 条新消息")

    // A1/A2/A3: search an old message outside the window, jump to it, come back to where you were.
    await page.locator("[data-history-toggle]").click()
    const panel = page.locator(width === 390 ? '[data-history="sheet"]' : '[data-history="aside"]')
    await panel.waitFor()
    await panel.locator("[data-history-topic]").first().waitFor()
    if (shots) await page.screenshot({ path: `${shots}/topics-${width}-${theme}.png`, animations: "disabled" })
    await panel.locator("[data-history-search]").fill("定位目标")
    const hit = panel.locator('[data-history-hit="o50"]')
    await hit.waitFor()
    assert.equal(await hit.locator("mark").textContent(), "定位目标")
    if (shots) await page.screenshot({ path: `${shots}/search-${width}-${theme}.png`, animations: "disabled" })
    await hit.click()
    const target = page.locator('[data-msg="o50"]')
    await page.locator('[data-msg="o50"][data-flash]').waitFor()
    await page.waitForFunction(() => {
      const target = document.querySelector('[data-msg="o50"]')?.getBoundingClientRect()
      const viewport = document.querySelector("[data-chat-scroll]")?.getBoundingClientRect()
      return target && viewport && Math.abs(target.y + target.height / 2 - (viewport.y + viewport.height / 2)) < 60
    })
    if (shots) await page.screenshot({ path: `${shots}/jump-${width}-${theme}.png`, animations: "disabled" })
    const box = await target.boundingBox(), view = await scroller.boundingBox()
    assert.ok(box.y >= view.y && box.y + box.height <= view.y + view.height, "the hit is in view")
    assert.ok(Math.abs(box.y + box.height / 2 - (view.y + view.height / 2)) < 60, "the hit is centred")
    assert.ok(await page.locator("[data-msg]").count() < 24, "only the visible rows are mounted")
    assert.equal(calls.filter((c) => c.startsWith("/api/views/page?kind=message")).length, 3)
    // The jump can finish before the sheet's exit animation unmounts it.
    if (width === 390) await page.locator('[data-history="sheet"]').waitFor({ state: "detached", timeout: 3000 })
    await page.locator("[data-jump-back]").click()
    await waitForAnchor(reading)
    assert.ok(near(await offsetOf(reading.id), reading.off), "back to where you were")
    assert.equal(await page.locator("[data-jump-bar]").count(), 0)

    // From the latest: the bar offers only the way down.
    await page.locator("[data-new-messages]").click()
    await page.waitForTimeout(150)
    assert.ok(await atBottom())
    if (width === 390) await page.locator("[data-history-toggle]").click()
    await page.locator("[data-history-search]").fill("st-7")
    await page.locator('[data-history-hit="o51"]').click()
    await page.locator("[data-jump-bar]").waitFor()
    assert.equal(await page.locator("[data-jump-back]").count(), 0)
    await page.locator("[data-jump-latest]").click()
    await page.waitForTimeout(150)
    assert.ok(await atBottom())
    if (width !== 390) await page.locator("[data-history-close]").click()

    // A6: older messages come in above; what you read stays put.
    await page.reload()
    await page.locator('[data-msg="w499"]').waitFor()
    await page.waitForTimeout(250)
    await page.mouse.move(width / 2, 400)
    const pageCount = () => calls.filter((c) => c.startsWith("/api/views/page?kind=message")).length
    const pagesBefore = pageCount()
    const olderResponse = Promise.withResolvers()
    holdOlder = olderResponse.promise
    for (let n = 0; n < 40 && pageCount() === pagesBefore; n++) { await page.mouse.wheel(0, -2500); await page.waitForTimeout(120) }
    assert.ok(pageCount() > pagesBefore, "scrolling loads an earlier page")
    const top = await anchor()
    assert.ok(top?.id.startsWith("w"))
    holdOlder = null
    olderResponse.resolve()
    await scroller.locator('[data-chat-older="more"]').waitFor({ state: "attached" })
    await waitForAnchor(top)
    assert.ok(!(await atBottom()), "loading older messages must not resume following the latest reply")
    const prepended = await offsetOf(top.id)
    assert.ok(near(prepended, top.off), `prepending shifted ${top.id}: ${top.off} → ${prepended}`)
    await page.mouse.wheel(0, -600)
    await page.locator('[data-msg^="o"]').first().waitFor({ state: "attached" })

    // A7: the reading position and the draft survive a reload; at the bottom it stays the bottom.
    await page.mouse.wheel(0, 1200)
    await page.waitForTimeout(500)
    const before = await anchor()
    await page.locator('[data-composer="chat"] textarea').fill("还没发出去的话")
    await page.waitForTimeout(400)
    await page.reload()
    await page.locator(`[data-msg="${before.id}"]`).waitFor({ state: "attached" })
    await waitForAnchor(before, 8)
    const restored = await offsetOf(before.id)
    assert.ok(near(restored, before.off, 8), `restored ${before.id}: ${restored} vs ${before.off}, saved ${await page.evaluate(() => localStorage.getItem("steve:read:v1:ws-history"))}, now ${JSON.stringify(await anchor())}`)
    assert.equal(await page.locator('[data-composer="chat"] textarea').inputValue(), "还没发出去的话")
    await page.locator("[data-jump-latest]").click()
    await page.waitForTimeout(500)
    await page.reload()
    await page.locator('[data-msg="w499"]').waitFor()
    await page.waitForTimeout(400)
    assert.ok(await atBottom())

    // A9: the IM conversations are one row away in 历史.
    await page.locator("[data-history-toggle]").click()
    await page.locator("[data-open-conversations]").click()
    await page.locator("[data-conversation-manager]").waitFor()
    await page.keyboard.press("Escape")

    // A12: nothing wider than the screen.
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × dark/light", "compact bar", "new message keeps the view", "search with highlight", "jump across the window with fill", "back to where you were", "latest only from the bottom", "older messages keep the anchor", "reading position and draft after reload", "bottom after reload", "IM conversations from 历史", "no horizontal overflow"] }))
} finally { await browser.close() }
