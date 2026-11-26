// Vite-only fixture for #/sessions: verifies per-session cumulative tokens + USD cost on rows
// and live memory + disk written bytes on active sessions across desktop & mobile, light & dark.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const errors = []

try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 960 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.goto(base + `/test/fixtures/sessions-showcase.html${theme === "dark" ? "?dark" : ""}`)
    const liveRow = page.locator('[data-session-row="s0"]')
    await liveRow.waitFor()
    assert.match((await liveRow.locator("[data-session-cost]").innerText()).replace(/\s+/g, " "), /45K · \$0\.18/)
    assert.match((await liveRow.locator("[data-session-vitals]").innerText()).replace(/\s+/g, " "), /内存 256 MB · 写 40 MB/)

    const waitingRow = page.locator('[data-session-row="s1"]')
    await waitingRow.waitFor()
    assert.match((await waitingRow.locator("[data-session-cost]").innerText()).replace(/\s+/g, " "), /90K · \$0\.36/)
    assert.equal(await waitingRow.locator("[data-session-vitals]").count(), 0, "waiting non-running session has no live vitals")

    // Scroll the first history day group into view so VirtualList mounts ended session s3.
    await page.locator('[data-sessions-group]').nth(1).scrollIntoViewIfNeeded()
    const endedRow = page.locator('[data-session-row="s3"]')
    await endedRow.waitFor()
    assert.match((await endedRow.locator("[data-session-cost]").innerText()).replace(/\s+/g, " "), /180K · \$0\.72/)
    assert.equal(await endedRow.locator("[data-session-vitals]").count(), 0, "ended session has no live vitals")

    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no horizontal overflow at ${width}px ${theme}`)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "session rows show tokens + USD cost", "active rows show live memory + disk written bytes", "no horizontal overflow"] }))
} finally { await browser.close() }
