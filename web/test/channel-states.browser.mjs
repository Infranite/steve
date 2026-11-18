import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
try {
  for (const width of [390, 1280]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 844 } })
    const errors = []
    page.on("pageerror", e => errors.push(e.message))
    await page.route("**/api/**", route => {
      if (!new URL(route.request().url()).pathname.startsWith("/api/")) return route.continue()
      throw new Error(`unexpected API ${route.request().url()}`)
    })
    await page.goto(base + "/test/fixtures/channel-states.html")
    await page.locator("[data-summary]").waitFor()
    const text = name => page.locator(`[data-case="${name}"]`).innerText()
    assert.match(await text("eligibility"), /未发送到聊天.*未核实/)
    assert.match(await text("blocked"), /渠道限制，未发送.*target_forbidden \(-32016\)/)
    assert.match(await text("draft"), /等你批准后发出/)
    assert.match(await text("unknown"), /可能已经发出，正在核对/)
    assert.equal(await text("sent"), "已发出")
    assert.equal(await text("silent-skip"), "")
    for (const name of ["reaction-cleared", "reaction-done-rejected"]) {
      assert.match(await text(name), /表情状态未确认.*target_forbidden \(-32016\)/)
      assert.doesNotMatch(await text(name), /已回答|已发出/)
    }
    assert.equal(await text("reaction-accepted"), "已处理")
    assert.equal(await page.locator("[data-summary]").innerText(), "只读到了部分片段，无法覆盖最近两天。")
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("channel states: generated/eligibility/blocked/draft/unknown/sent and rejected reactions pass at 390/1280 px")
} finally { await browser.close() }
