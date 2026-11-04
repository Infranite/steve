import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const errors = []
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) for (const hash of ["", "#down"]) {
    const page = await browser.newPage({ viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    await page.route(u => u.pathname.startsWith("/api/"), route => route.fulfill({ status: 503, json: { error: "fixture" } }))
    await page.goto(base + "/test/fixtures/lane.html" + hash)
    const lane = page.locator("[data-lane]")
    await lane.locator("[data-lane-merge]").waitFor()
    assert.match(await lane.locator("[data-lane-target]").innerText(), /github\.com\/infranite\/steve#main/)
    assert.match(await lane.locator("[data-lane-ship]").innerText(), /推送版本 tag[\s\S]*完成标准是 PR 合并并推送 tag/)
    assert.equal(await lane.locator("[data-lane-numbered]").count(), 4)
    if (hash) {
      // couldn't look: said so, with why, never "nothing there"
      assert.match(await lane.locator("[data-lane-merge]").innerText(), /查不到[\s\S]*Bad credentials/)
      assert.match(await lane.locator("[data-lane-target]").innerText(), /没取到最新状态/)
    } else {
      assert.match(await lane.locator("[data-lane-merge]").innerText(), /直接推送到分支[\s\S]*没有要求 PR/)
      assert.match(await lane.locator("[data-lane-checks]").innerText(), /合并前要过 · 没有[\s\S]*没找到，按默认/)
    }
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "the section never widens the page")
    await lane.scrollIntoViewIfNeeded()
    await page.waitForTimeout(300)
    if (shots) await lane.screenshot({ path: `${shots}/lane-${width}-${theme}${hash ? "-down" : ""}.png`, animations: "disabled" })
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "found / couldn't look", "no horizontal overflow"] }))
} finally { await browser.close() }
