import assert from "node:assert/strict"
/* #40: an acceptance card of a project with a release offers 验收并发布 and 验收，先不发布; without one, plain 验收通过 */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5195"
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 844 } }), errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", (route) => { const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (u.pathname.startsWith("/api/")) return route.fulfill({ status: 503, json: { error: "fixture" } }); return route.continue() })
    await page.goto(base + "/test/fixtures/accept-release.html"); await page.locator("[data-pass-release]").waitFor()
    assert.equal(await page.locator("[data-pass-release]").innerText(), "验收并发布")
    assert.equal(await page.locator("[data-pass-no-release]").innerText(), "验收，先不发布")
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
    await page.locator("[data-pass-release]").click()
    await page.getByText("已验收，开始发布").waitFor()
    assert.deepEqual(await page.evaluate(() => window.__calls), [{ id: "dec_1", choice: "pass", text: "release" }])
    await page.goto(base + "/test/fixtures/accept-release.html?again"); await page.locator("[data-pass-no-release]").waitFor()
    await page.evaluate(() => { window.__done = true })
    await page.locator("[data-pass-no-release]").click()
    await page.getByText("已验收，这次不发布").waitFor()
    assert.deepEqual(await page.evaluate(() => window.__calls), [{ id: "dec_1", choice: "pass", text: "no-release" }])
    // a project without a release: one button, as before
    await page.goto(base + "/test/fixtures/accept-release.html?norelease"); await page.locator("[data-pass]").waitFor()
    assert.equal(await page.locator("[data-pass-release]").count(), 0)
    assert.equal(await page.locator("[data-pass]").innerText(), "验收通过")
    // a project done by its merge rule: the rule is the completion, nothing to choose on the card
    await page.goto(base + "/test/fixtures/accept-release.html?donewhen"); await page.locator("[data-pass]").waitFor()
    assert.equal(await page.locator("[data-pass-release]").count(), 0)
    assert.equal(await page.locator("[data-pass-no-release]").count(), 0)
    assert.equal(await page.locator("[data-pass]").innerText(), "验收通过")
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("accept-release ok")
} finally { await browser.close() }
