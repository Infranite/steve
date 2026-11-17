// Production collaboration controls, isolated Vite fixture; never calls a live Hub.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
try {
  for (const width of [1066, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 950 } })
    page.on("pageerror", e => errors.push(e.message))
    await page.route(`${base}/api/**`, route => route.fulfill({ json: {} }))
    await page.goto(base + "/test/fixtures/workflow.html?disabled")
    await page.locator("[data-workflow-disabled]").waitFor()
    assert.match(await page.locator("[data-workflow-disabled]").innerText(), /已停用动态工作流/)
    assert.equal(await page.locator("[data-workflow-unavailable]").count(), 0)
    await page.locator('[data-agent-node="unit:review"]').click()
    assert.equal(await page.locator('[data-agent-panel] [data-workflow-retry="review"]').isDisabled(), true)
    await page.locator("[data-agent-detail-close]").click()
    await page.locator("[data-agent-panel]").waitFor({ state: "detached" })
    await page.locator('[data-agent-node="unit:api"]').click()
    assert.equal(await page.locator('[data-workflow-rework="api"]').isDisabled(), true)
    assert.match(await page.locator("[data-workflow-outputs]").innerText(), /工作图 API/)
    await page.locator("[data-agent-detail-close]").click()
    await page.locator("[data-agent-panel]").waitFor({ state: "detached" })
    assert.equal(await page.locator("[data-workflow-toggle]").isDisabled(), false, "pause remains available")
    await page.locator("[data-workflow-toggle]").click()
    assert.equal(await page.locator("[data-workflow-toggle]").isDisabled(), true, "resume stays blocked")
    await page.locator("[data-agent-more]").click()
    await page.locator("[data-workflow-single]").click()
    assert.match(await page.locator("[data-workflow-single-confirm]").innerText(), /明确豁免本轮剩余/)
    await page.locator('[data-slot="alert-dialog-cancel"]').click()
    await page.locator("[data-workflow-single-confirm]").waitFor({ state: "detached" })
    assert.deepEqual(await page.evaluate(() => (window.__workflowChanges ?? []).map(c => c.action)), ["pause"], "confirmation preview never waives acceptance")
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/workflow-disabled-${width}.png`, fullPage: true })
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("disabled workflow controls passed at desktop and phone widths")
} finally {
  await browser.close()
}
