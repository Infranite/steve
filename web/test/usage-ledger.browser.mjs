// The actual session panel, item usage row and statistics drill-down must agree on all four states.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:18778"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
try {
  for (const [state, expected] of [["not_reported", "尚未上报"], ["unavailable", "用量不可用"], ["partial", "≥12 · 部分覆盖"], ["complete", "0"]]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1440, height: 1000 } })
    const errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", (r) => new URL(r.request().url()).pathname.startsWith("/api/") ? r.abort("blockedbyclient") : r.continue())
    await page.goto(`${base}/test/fixtures/session-panel.html?coverage=${state}`)
    const session = page.locator("[data-session-usage] dd")
    const item = page.locator("[data-item-usage-text]")
    await session.waitFor(); await item.waitFor()
    assert.ok((await session.innerText()).includes(expected), `${state}: session`)
    assert.ok((await item.innerText()).includes(expected), `${state}: item`)
    const tokenKpi = page.locator("[data-drill] [data-kpi=tokens]")
    await tokenKpi.waitFor()
    assert.ok((await tokenKpi.innerText()).includes(expected), `${state}: stats`)
    const waterfall=page.locator("[data-cost-waterfall] .ui-cwfc__stats")
    assert.ok((await waterfall.innerText()).includes(expected), `${state}: waterfall`)
    if (state==="not_reported" || state==="unavailable") assert.ok(!(await waterfall.innerText()).includes("$0"), `${state}: unknown cost is not zero`)
    assert.equal(errors.length, 0, errors.join("\n"))
    if (shots) await page.screenshot({ path: `${shots}/usage-${state}.png`, fullPage: true })
    await page.close()
  }
  console.log("PASS: session, item and stats agree for not_reported/unavailable/partial/zero")
} finally {
  await browser.close()
}
