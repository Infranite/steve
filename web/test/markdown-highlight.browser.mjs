import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
try {
  for (const blocked of [false, true]) {
    const page = await browser.newPage()
    let requests = 0
    const errors = []
    page.on("pageerror", (error) => errors.push(error.message))
    await page.route("**/markdown-highlight.worker.ts*", (route) => {
      requests++
      return blocked ? route.abort() : route.continue()
    })
    await page.goto(base + "/test/fixtures/markdown-highlight.html")
    const code = page.locator("[data-streamdown=code-block-body] pre code")
    await code.waitFor()
    assert.match(await code.textContent(), /const result = 42/)
    assert.equal(requests, 0, "an unfinished fence renders without a syntax worker")
    const failed = blocked && page.waitForEvent("requestfailed", { predicate: (r) => r.url().includes("markdown-highlight.worker.ts") })
    await page.getByRole("button", { name: "Close fence" }).click()
    if (!blocked) await code.locator('[style*="--sdm-c"]').first().waitFor()
    else await failed
    assert.match(await code.textContent(), /const result = 42/)
    assert.equal(requests, 1)
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("raw streaming, colored closed fences and a blocked worker fallback")
} finally {
  await browser.close()
}
