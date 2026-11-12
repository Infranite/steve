// Vite-only fixture; external Playwright prerequisite, no API calls.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
try {
  for (const width of [1066, 390]) for (const theme of ["dark", "light"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    await page.goto(base + "/test/fixtures/item-card.html")
    await page.locator('[data-item-card="ST-41"]').waitFor()
    await page.waitForTimeout(300)
    for (const id of ["ST-33", "ST-40", "ST-41", "ST-43"]) {
      const card = page.locator(`[data-item-card="${id}"]`), box = await card.boundingBox()
      // nothing on the card reaches past its edge, and the title keeps its room beside the status
      const over = await card.evaluate(el => { const r = el.getBoundingClientRect(); return [...el.querySelectorAll("*")].filter(e => e.getBoundingClientRect().right > r.right + 1).map(e => e.outerHTML.slice(0, 80)) })
      assert.deepEqual(over, [], `${id} overflows its card at ${width}px`)
      const title = await card.locator("[data-item-ref] .type-body").boundingBox()
      assert.ok(title.width > box.width / 3, `${id} title squeezed to ${title.width}px`)
    }
    // the misfiled build sentence is a build line, not “已发布 …”
    const legacy = page.locator('[data-item-card="ST-33"]')
    assert.equal((await legacy.locator("[data-card-status]").innerText()).trim(), "已完成")
    assert.match(await legacy.locator("[data-card-build]").innerText(), /Release 构建失败/)
    assert.doesNotMatch(await legacy.locator("[data-card-build]").innerText(), /^ST-33/)
    // the link is the URL alone, the sentence after it stays text
    assert.equal(await legacy.locator("[data-card-build] a").getAttribute("href"), "https://github.com/Infranite/steve/actions/runs/41851954210")
    // an item closed as failed is over, like done and dropped: not in progress, not waiting on you
    const failedCard = page.locator('[data-item-card="ST-43"]')
    assert.equal(await failedCard.getAttribute("data-card-state"), "fail")
    assert.equal((await failedCard.locator("[data-card-status]").innerText()).trim(), "已失败")
    const built = page.locator('[data-item-card="ST-40"]')
    await built.locator("[data-card-build]").waitFor()
    if (width > 640) assert.match(await built.locator("[data-card-status]").innerText(), /已发布\s*v0\.2\.24/)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/item-card-${width}-${theme}.png`, fullPage: true })
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["legacy build-as-version card", "build line", "long version truncates", "closed as failed", "no overflow", "title keeps room", "1066/390px", "dark/light"] }))
} finally { await browser.close() }
