/* Personal reply reading in the real scroll viewport: hidden and offscreen content stays unread. */
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5297"
const errors = [], reads = []
try {
  const page = await browser.newPage({ viewport: { width: 1240, height: 820 }, locale: "zh-CN" })
  page.on("pageerror", e => errors.push(e.message))
  // Withhold focus until the test has deliberately scrolled up. Opening the app cannot clear later replies.
  await page.addInitScript(() => { window.fixtureFocused = false; document.hasFocus = () => window.fixtureFocused })
  await page.route("**/api/**", route => {
    const u = new URL(route.request().url())
    if (!u.pathname.startsWith("/api/")) return route.continue()
    if (u.pathname === "/api/me/attention") return route.fulfill({ json: {
      v: 1, epoch: "test", user_id: "reader", workspace_id: "ws-history", history_policy: "since_attention_v1",
      categories: [{ kind: "replies", available: true, unchanged: false, revision: "v1", count: 100, unit: "messages", snapshot: "snapshot",
        entries: Array.from({ length: 100 }, (_, n) => ({ id: "w" + (301 + n * 2), event_id: "reply-" + n, order: n + 1, created: n + 1, target: { route: "", entity_id: "w" + (301 + n * 2) }, notify: true })), next_cursor: null }],
    } })
    if (u.pathname === "/api/me/attention/read") { reads.push(route.request().postDataJSON()); return route.fulfill({ json: { updated: 1 } }) }
    return route.fulfill({ status: 404, json: {} })
  })
  await page.goto(base + "/test/fixtures/chat-history.html")
  const scroll = page.locator("[data-chat-scroll]")
  await page.locator('[data-msg="w499"]').waitFor({ state: "attached" })
  await page.waitForTimeout(500)
  assert.equal(reads.length, 0, "unfocused app must not mark replies read")
  await page.mouse.move(600, 400); await page.mouse.wheel(0, -2200)
  await page.waitForTimeout(400)
  const visible = await scroll.evaluate(el => {
    const box = el.getBoundingClientRect()
    return [...el.querySelectorAll("[data-msg]")].filter(x => { const r = x.getBoundingClientRect(); return r.bottom > box.top && r.top < box.bottom }).map(x => x.dataset.msg)
  })
  assert.ok(!visible.includes("w499"), "latest final reply is deliberately offscreen")
  await page.evaluate(() => { window.fixtureFocused = true; window.dispatchEvent(new Event("focus")) })
  await page.waitForFunction(() => true); await page.waitForTimeout(700)
  assert.ok(reads.length > 0, "focused visible final reply was read")
  assert.ok(visible.includes(reads.at(-1).through_id), JSON.stringify({ visible, reads }))
  assert.notEqual(reads.at(-1).through_id, "w499")
  await page.evaluate(() => { window.fixtureFocused = false })
  const before = reads.length
  await scroll.evaluate(el => { el.scrollTop = el.scrollHeight })
  await page.waitForTimeout(600)
  assert.equal(reads.length, before, "scrolling while unfocused does not advance reading")
  if (process.env.STEVE_TEST_SHOT) await page.screenshot({ path: process.env.STEVE_TEST_SHOT })
  assert.deepEqual(errors, [])
  console.log("PASS focused viewport reads only actual final reply; background app preserves unread")
} finally { await browser.close() }
