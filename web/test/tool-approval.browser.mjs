import assert from "node:assert/strict"
/* a tool permission answered with Allow once, Always (the checkbox) and Deny posts the harness's own option ids; Esc lets go
 * of the card without denying. A plain approval rejects from its field, with or without a reason */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5195"
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 844 } }), errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", (route) => { const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (u.pathname.startsWith("/api/")) return route.fulfill({ status: 503, json: { error: "fixture" } }); return route.continue() })
    const open = async (q = "") => { await page.goto(base + "/test/fixtures/tool-approval.html" + q); await page.locator("[data-decision-id]").first().waitFor() }
    const calls = () => page.evaluate(() => JSON.parse(JSON.stringify(window.__calls)))
    await open()
    const card = page.locator('[data-decision-id="dec_1"]')
    assert.equal(await card.getAttribute("data-decision"), "approve")
    assert.equal(await card.getAttribute("data-decision-wrap"), "ST-8")
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
    // Esc inside the card only lets go of it
    await card.locator('input[type="checkbox"]').focus(); await page.keyboard.press("Escape"); await page.waitForTimeout(150)
    assert.deepEqual(await calls(), [])
    await card.locator("[data-allow]").click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_1", choice: "opt_once" }])
    await open("?always")
    await page.locator('[data-decision-id="dec_1"] input[type="checkbox"]').check()
    await page.locator("[data-allow]").click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_1", choice: "opt_always" }])
    await open("?deny")
    await page.locator("[data-deny]").click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_1", choice: "deny" }])
    // a plain approval: one 不批准, in the field, that sends without a reason or with one
    await open("?approve")
    assert.equal(await page.locator("[data-reject]").count(), 1)
    await page.locator("[data-reject]").click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_2", choice: "no" }])
    await open("?approve&why")
    await page.locator("[data-reject-reason]").fill("先别发")
    await page.locator("[data-reject]").click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_2", choice: "no", text: "先别发" }])
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("tool-approval ok")
} finally { await browser.close() }
