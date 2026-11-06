// Vite fixture: exercises dependency display and revision-bearing actions without a live provider.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
try {
  for (const width of [1066, 390]) for (const theme of ["dark", "light"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 950 }, colorScheme: theme })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route(/\/api\/[^/]+$/, (route) => {
      if (new URL(route.request().url()).pathname.startsWith("/api/")) return route.fulfill({ json: {} })
      return route.continue()
    })
    await page.goto(base + "/test/fixtures/workflow.html")
    await page.locator("[data-workflow]").waitFor()
    assert.deepEqual(await page.locator("[data-workflow-unit]").evaluateAll((els) => els.map((e) => e.dataset.workflowUnit)), ["api", "web", "review", "integrate"])
    assert.match(await page.locator("[data-workflow-blockers]").innerText(), /主事项暂不能完成/)
    await page.locator('[data-workflow-unit="api"] summary').click()
    await page.locator("[data-workflow-outputs]").waitFor()
    assert.match(await page.locator('[data-workflow-unit="api"]').innerText(), /实际操作/)
    await page.locator("[data-workflow-toggle]").click()
    await page.getByText("编排已暂停，恢复后按当前计划继续。").waitFor()
    await page.locator("[data-workflow-toggle]").click()
    await page.getByRole("button", { name: "暂停编排" }).waitFor()
    await page.locator('[data-workflow-retry="review"]').click()
    assert.deepEqual(await page.evaluate(() => window.__workflowChanges.map(({ action, expected_revision }) => ({ action, expected_revision }))), [
      { action: "pause", expected_revision: 7 }, { action: "resume", expected_revision: 8 }, { action: "retry", expected_revision: 9 },
    ])
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/workflow-${width}-${theme}.png`, fullPage: true })
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["dependency order", "acceptance blockers", "shared outputs", "pause/resume/retry revisions", "1066/390px", "dark/light"] }))
} finally { await browser.close() }
