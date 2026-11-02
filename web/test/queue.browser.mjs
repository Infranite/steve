// Development fixture; no real backend calls are permitted.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const page = await browser.newPage({ viewport: { width: 390, height: 844 } })
const errors = [], requests = []
page.on("pageerror", e => errors.push(e.message))
await page.route("**/*", route => {
  const u = new URL(route.request().url())
  if (u.origin !== base || u.pathname.startsWith("/api/")) { requests.push(u.pathname); return route.abort("blockedbyclient") }
  return route.continue()
})
try {
  await page.goto(base + "/test/fixtures/queue.html")
  const normal = page.locator('[data-queued="normal"]')
  await normal.waitFor()
  for (const id of ["recovery", "agent"]) {
    const row = page.locator(`[data-queued="${id}"]`)
    assert.match(await row.innerText(), /重启前已送达，正在核对进度 \/ 等待续做/)
    assert.equal(await row.locator("[data-queued-edit], [data-queued-withdraw], [data-queued-send], [data-queued-save]").count(), 0)
  }
  assert.match(await page.locator("[data-sq-note]").innerText(), /待核对 2 条；排队 1 条/)
  assert.equal(await normal.locator("[data-queued-edit], [data-queued-withdraw], [data-queued-send]").count(), 3)
  await normal.locator("[data-queued-edit]").click()
  await normal.locator("[data-queued-input]").fill("草稿修改中")
  assert.equal(await normal.locator("[data-queued-save]").count(), 1)
  await page.getByRole("button", { name: "模拟队列恢复标记" }).click()
  await normal.locator("[data-queued-recovery-note]").waitFor()
  assert.equal(await normal.locator("[data-queued-input], [data-queued-save], [data-queued-edit], [data-queued-withdraw], [data-queued-send]").count(), 0, "a row becoming resumed cannot keep stale edit controls")
  assert.match(await normal.innerText(), /普通排队消息/)
  const note = await page.locator("[data-sq-note]").innerText()
  assert.equal(note, "待核对 3 条")
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
  assert.deepEqual(errors, [])
  assert.deepEqual(requests, [], "viewing and switching a recovery marker must not submit operations")
  console.log(JSON.stringify({ passed: true, scenarios: ["resumed user", "resumed agent", "normal controls retained", "mixed header counts", "edit-to-resumed transition", "recovery-only header", "390px", "no network mutations"] }))
} finally { await browser.close() }
