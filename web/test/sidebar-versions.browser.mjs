// Vite-only fixture; external Playwright prerequisite, no API calls.
// ST-21: the bottom of the sidebar names this page's frontend and the Hub's backend, and links to 设置 · 关于.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const errors = []
try {
  for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route((u) => u.pathname.startsWith("/api/"), (route) => route.fulfill({ status: 503, json: { error: "fixture" } }))
    await page.goto(base + "/test/fixtures/sidebar-versions.html")
    const v = page.locator("[data-sidebar-versions]")
    await v.waitFor()
    assert.equal((await v.innerText()).trim(), "前端 4d419d6 · 后端 0.2.45")
    assert.equal(await v.getAttribute("href"), "#/settings/about")
    assert.equal(await v.getAttribute("title"), "前端 20261105-120301-4d419d6a · 后端 0.2.45")
    if (shots) await page.screenshot({ path: `${shots}/sidebar-versions-${theme}.png`, animations: "disabled" })
    await page.close()
  }
  // not known yet: says so instead of an empty name
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
  await page.route((u) => u.pathname.startsWith("/api/"), (route) => route.fulfill({ status: 503, json: { error: "fixture" } }))
  await page.goto(base + "/test/fixtures/sidebar-versions.html?hub=")
  assert.equal((await page.locator("[data-sidebar-versions]").innerText()).trim(), "前端 4d419d6 · 后端 …")
  await page.close()
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["light/dark", "frontend by commit, backend by version", "links to 设置 · 关于", "backend not known yet"] }))
} finally { await browser.close() }
