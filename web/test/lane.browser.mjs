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
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    await page.route(u => u.pathname.startsWith("/api/"), route => route.fulfill({ status: 503, json: { error: "fixture" } }))
    await page.goto(base + "/test/fixtures/lane.html" + hash)
    const where = page.locator("[data-proj-where]")
    await where.locator('[data-fact="merge"]').waitFor()
    assert.match(await where.locator('[data-fact="repo"]').innerText(), /github\.com\/infranite\/steve · 分支 main/)
    assert.match(await where.locator('[data-fact="ship"]').innerText(), /推送版本 tag[\s\S]*完成标准：PR 合并并推送 tag/)
    // the conventions are folded; opened, each check, file and source is listed once
    const conv = page.locator("[data-lane-conventions]")
    assert.match(await conv.innerText(), /仓库约定\s*·\s*2 项检查\s*·\s*4 处要协调/)
    await conv.locator("button").first().click()
    await conv.locator("[data-lane-checks]").waitFor()
    assert.equal(await conv.locator("[data-lane-numbered]").count(), 4)
    assert.equal(await conv.locator("[data-lane-check]").count(), 2)
    assert.equal(await page.getByText("hub-rs/Cargo.toml 是 Rust 工作区").count(), 0, "a check's evidence is not listed again")
    assert.match(await conv.innerText(), /不另跑检查/)
    if (hash) {
      // couldn't look: said so, with why, never "nothing there"
      assert.match(await where.locator('[data-fact="merge"]').innerText(), /直接推送到分支[\s\S]*查不到[\s\S]*Bad credentials/)
      assert.match(await where.locator('[data-fact="repo"]').innerText(), /没取到最新状态/)
      assert.match(await conv.innerText(), /合并前的检查[\s\S]*查不到/)
      // a lane nothing went into yet: nothing in flight on the overview, the limits still there to set
      assert.equal(await page.locator("[data-lane-batches]").count(), 0)
      await page.locator('[data-seg="settings"]').click()
      assert.equal(await page.locator("[data-lane-limits]").count(), 1)
      await page.locator('[data-seg="overview"]').click()
    } else {
      assert.match(await where.locator('[data-fact="merge"]').innerText(), /直接推送到分支/)
      assert.match(await conv.innerText(), /合并前的检查[\s\S]*没找到，按默认/)
      assert.match(await conv.locator('[data-fact="lane-merge"]').innerText(), /没有要求 PR/)
      // the lane itself: what is in flight and what collects sit with the items, the way out of the window with them
      const lane = page.locator("[data-lane-batches]")
      assert.match(await lane.locator("[data-lane-running]").innerText(), /MQ-3 · 正在合并[\s\S]*ST-60/)
      assert.match(await lane.locator("[data-lane-collecting]").innerText(), /MQ-4 · 合批中[\s\S]*ST-61[\s\S]*ST-62/)
      if (width === 1440 && theme === "light") {
        await page.locator('[data-seg="settings"]').click()
        await page.locator("[data-lane-window]").fill("15")
        await page.locator("[data-lane-save]").click()
        await page.waitForFunction(() => window.sent.length === 1)
        assert.deepEqual(await page.evaluate(() => window.sent[0]), { window_min: 15, max_members: null })
        await page.locator('[data-seg="overview"]').click()
        await lane.locator("[data-lane-now]").click()
        await lane.locator("[data-lane-now]").waitFor({ state: "detached" })
        assert.match(await lane.locator("[data-lane-collecting]").innerText(), /MQ-4 · 已截止/)
      }
    }
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "the section never widens the page")
    await page.waitForTimeout(300)
    if (shots) await page.screenshot({ path: `${shots}/lane-${width}-${theme}${hash ? "-down" : ""}.png`, animations: "disabled" })
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "found / couldn't look", "conventions folded, listed once", "no horizontal overflow"] }))
} finally { await browser.close() }
