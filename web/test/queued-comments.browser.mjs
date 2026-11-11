// Vite-only fixture; external Playwright prerequisite, no API calls.
// ST-39: on an item page a queued supplement shows on its comment, not a second time above the composer.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = [], requests = []
try {
  for (const width of [1066, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 }, colorScheme: "dark" })
    page.on("pageerror", e => errors.push(e.message))
    await page.route("**/*", route => {
      const u = new URL(route.request().url())
      if (u.origin !== base || u.pathname.startsWith("/api/")) { requests.push(u.pathname); return route.abort("blockedbyclient") }
      return route.continue()
    })
    await page.goto(base + "/test/fixtures/queued-comments.html")
    await page.locator('[data-queued="q1"]').waitFor()
    /* the two comments still in the queue carry the queue state and its controls */
    for (const [id, n] of [["q1", 0], ["q2", 3]]) {
      const c = page.locator(`[data-comment="${n}"] [data-queued="${id}"][data-queued-comment]`)
      assert.equal(await c.count(), 1, `${id} shows on comment ${n}`)
      assert.match(await c.innerText(), /排队中，本回合结束后发送/)
      assert.equal(await c.locator("[data-queued-edit], [data-queued-withdraw], [data-queued-send]").count(), 3)
    }
    assert.equal(await page.locator('[data-comment="3"] [data-streamdown="strong"]').innerText(), "第二条", "queued comments keep their Markdown")
    assert.equal(await page.locator('[data-comment="2"] [data-queued]').count(), 0, "an answered comment is not queued")
    /* the strip keeps only what no comment claims: Steve's row and the unmatched one */
    const strip = page.locator("[data-running-strip] [data-sq]")
    assert.deepEqual(await strip.locator("li[data-queued]").evaluateAll(xs => xs.map(x => x.getAttribute("data-queued"))), ["q3", "q4"])
    /* before the session is up: Steve's brief is not "a queued message" and has no controls; nothing offers 立即发送 */
    const brief = page.locator("[data-starting-brief] [data-sq]")
    assert.match(await brief.locator("[data-sq-note]").innerText(), /^任务说明 · 会话启动后开始$/)
    assert.equal(await brief.locator("button[data-queued-edit], button[data-queued-withdraw], button[data-queued-send]").count(), 0)
    const mixed = page.locator("[data-starting-mixed] [data-sq]")
    assert.match(await mixed.locator("[data-sq-note]").innerText(), /排队 2 条 · 会话启动后发送/)
    assert.equal(await mixed.locator('[data-queued="b1"] button').count(), 0, "the brief only waits")
    assert.equal(await mixed.locator('[data-queued="b2"]').locator("[data-queued-edit], [data-queued-withdraw]").count(), 2)
    assert.equal(await mixed.locator("[data-queued-send]").count(), 0, "no turn to interrupt yet")
    /* a start that stands still shows: the time it has taken, and past two minutes that this is long */
    assert.equal(await page.locator("[data-start-fresh] [data-starting]").innerText(), "正在开始…")
    assert.match(await page.locator("[data-start-long] [data-starting]").innerText(), /^正在开始…\s*5:0\d\s*比平时久。/)
    assert.equal(await page.locator("[data-start-fresh] [data-starting-slow]").count(), 0)
    /* editing happens in place on the comment */
    const q1 = page.locator('[data-queued="q1"]')
    await q1.locator("[data-queued-edit]").click()
    assert.equal(await q1.locator("[data-queued-input]").inputValue(), "这里不支持markdown渲染。举一反三，都检查下")
    await q1.getByRole("button", { name: "取消" }).click()
    assert.equal(await q1.locator("[data-queued-input]").count(), 0)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/queued-comments-${width}.png`, fullPage: true })
    await page.close()
  }
  assert.deepEqual(errors, [])
  assert.deepEqual(requests, [], "viewing queued comments must not call the API")
  console.log(JSON.stringify({ passed: true, scenarios: ["queued comment state", "controls on comment", "markdown kept", "answered comment untouched", "strip keeps unpaired rows only", "starting session: brief without controls, no send-now", "slow start shown", "inline edit", "1066/390px"] }))
} finally { await browser.close() }
