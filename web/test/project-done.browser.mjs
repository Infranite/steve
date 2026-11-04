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
    // no Hub behind the fixture: its own calls are stubbed in the page
    await page.route(u => u.pathname.startsWith("/api/"), route => route.fulfill({ status: 503, json: { error: "fixture" } }))
    await page.goto(base + "/test/fixtures/project-done.html")
    const row = page.locator("[data-proj-done]")
    await row.waitFor()
    assert.match(await row.innerText(), /验收后在本机发布上线才结束/)
    await row.locator('[data-select="proj-done"]').click()
    await page.getByRole("option", { name: "PR 合并并推送 tag" }).click()
    await page.waitForFunction(() => document.querySelector("[data-proj-done]")?.textContent?.includes("构建另行跟踪"))
    const calls = await page.evaluate(() => window.__calls)
    assert.deepEqual(calls.at(-1), { id: "prj_1", body: { release: { kind: "script", cmd: "/Users/example/.steve/current/ops/release.sh", done_when: "tag" } } })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "the row never widens the page")
    await row.scrollIntoViewIfNeeded()
    await page.waitForTimeout(300)
    if (shots) await page.screenshot({ path: `${shots}/project-done-${width}-${theme}.png`, animations: "disabled", fullPage: true })
    if (width === 1440 && theme === "light") {
      // back to the release on this machine: the rule is dropped, the release command stays
      await row.locator('[data-select="proj-done"]').click()
      await page.getByRole("option", { name: "本机发布上线" }).click()
      await page.waitForFunction(() => document.querySelector("[data-proj-done]")?.textContent?.includes("本机发布上线"))
      assert.deepEqual((await page.evaluate(() => window.__calls)).at(-1).body, { release: { kind: "script", cmd: "/Users/example/.steve/current/ops/release.sh" } })
    }
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "local → tag writes done_when", "tag → local drops it, keeps the command", "no horizontal overflow"] }))
} finally { await browser.close() }
