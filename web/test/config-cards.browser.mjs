import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const errors = []
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    const calls = []
    // the Hub: apply / undo / drop of a draft
    await page.route("**/api/config/**", route => { calls.push(new URL(route.request().url()).pathname); return route.fulfill({ json: { ok: true } }) })
    await page.goto(base + "/test/fixtures/config-cards.html")
    const cards = page.locator("[data-proposal]")
    await cards.nth(2).waitFor()
    assert.deepEqual(await cards.evaluateAll(els => els.map(e => e.getAttribute("data-proposal"))), ["draft", "draft", "applied"], "an applied card stays applied after a reload")
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "cards never widen the page")
    await page.waitForTimeout(300)
    if (shots) await page.screenshot({ path: `${shots}/config-${width}-${theme}.png`, animations: "disabled" })
    if (width === 1440 && theme === "light") {
      await cards.nth(0).locator("[data-drop]").click()
      await page.locator('[data-proposal="dropped"]').waitFor()
      await cards.nth(1).locator("[data-apply]").click()
      await page.waitForFunction(() => document.querySelectorAll('[data-proposal="applied"]').length === 2)
      await cards.nth(1).locator("[data-undo]").click()
      await page.waitForFunction(() => document.querySelectorAll('[data-proposal="draft"]').length === 1)
      assert.deepEqual(calls, ["/api/config/chg_quiet/drop", "/api/config/chg_dev/apply", "/api/config/chg_dev/undo"])
    }
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "several drafts in one reply", "applied state from meta", "drop / apply / undo call the Hub", "no horizontal overflow"] }))
} finally { await browser.close() }
