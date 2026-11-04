// Vite-only fixture; external Playwright prerequisite, no API calls.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
try {
  for (const width of [1066, 390]) for (const theme of ["dark", "light"]) {
    const page = await browser.newPage({ viewport: { width, height: 700 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    await page.goto(base + "/test/fixtures/item-rows.html")
    await page.locator('[data-item-row="ST-53"]').waitFor()
    const row = id => page.locator(`[data-item-row="${id}"]`)
    // waiting for your word without a card: the summary is the reason, no button to a card that isn't there
    assert.equal(await row("ST-49").locator("[data-item-now]").innerText(), "新周期，等你说要做什么")
    assert.equal(await row("ST-49").locator("[data-item-wait-go]").count(), 0)
    // running: what it does now
    assert.equal(await row("ST-48").locator("[data-item-now]").innerText(), "新周期，收到新消息，开始处理")
    // a real wait: what the card asks and the way to it, not the stale summary
    const r = row("ST-52")
    assert.match(await r.locator('[data-item-wait="approve"]').innerText(), /^审批：合并 PR #12 并打 tag 发版 等 2 件$/)
    assert.equal(await r.locator("[data-item-now]").count(), 0)
    assert.equal((await r.locator("[data-item-wait-go]").innerText()).trim(), "去审批")
    assert.equal((await row("ST-53").locator("[data-item-wait-go]").innerText()).trim(), "去回答")
    // nothing reaches past the row
    for (const id of ["ST-49", "ST-48", "ST-52", "ST-53"]) {
      const over = await row(id).evaluate(el => { const b = el.getBoundingClientRect(); return [...el.querySelectorAll("*")].filter(e => e.getBoundingClientRect().right > b.right + 1).map(e => e.outerHTML.slice(0, 80)) })
      assert.deepEqual(over, [], `${id} overflows at ${width}px`)
    }
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/item-rows-${width}-${theme}.png`, fullPage: true })
    // the button takes you to the item, where its card is
    await r.locator("[data-item-wait-go]").click()
    assert.equal(new URL(page.url()).hash, "#/items/ST-52")
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["new cycle waiting for your word", "running after take-up", "approval reason and entry", "question entry", "no overflow", "1066/390px", "dark/light"] }))
} finally { await browser.close() }
