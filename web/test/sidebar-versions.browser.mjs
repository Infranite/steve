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
const open = async (opts, query = "") => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, reducedMotion: "reduce", ...opts })
  page.on("pageerror", (e) => errors.push(e.message))
  // no Hub behind the fixture
  await page.route((u) => u.pathname.startsWith("/api/"), (route) => route.fulfill({ status: 503, json: { error: "fixture" } }))
  await page.goto(base + "/test/fixtures/sidebar-versions.html" + query)
  const v = page.locator("[data-sidebar-versions]")
  await v.waitFor()
  return { page, v }
}
try {
  for (const theme of ["light", "dark"]) {
    const { page, v } = await open({ locale: "zh-CN", colorScheme: theme })
    assert.equal((await v.innerText()).trim(), "客户端 4d419d6a · 服务端 0.2.53")
    assert.equal(await v.getAttribute("href"), "#/settings/about")
    assert.equal(await v.getAttribute("title"), "客户端 20261105-120301-4d419d6a · 服务端 0.2.53")
    assert.equal(await page.evaluate(() => document.documentElement.classList.contains("dark")), theme === "dark")
    // readable on its background, in both themes
    const [fg, bg] = await v.evaluate((el) => {
      const rgb = (c) => c.match(/[\d.]+/g).slice(0, 3).map(Number)
      let n = el, b = "rgba(0, 0, 0, 0)"
      while (n && /rgba\(0, 0, 0, 0\)|transparent/.test(b)) { b = getComputedStyle(n).backgroundColor; n = n.parentElement }
      return [rgb(getComputedStyle(el).color), rgb(b)]
    })
    const lum = ([r, g, b]) => [r, g, b].map((c) => { c /= 255; return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4 }).reduce((s, c, i) => s + c * [0.2126, 0.7152, 0.0722][i], 0)
    const [a, b] = [lum(fg), lum(bg)].sort((x, y) => y - x)
    assert.ok((a + 0.05) / (b + 0.05) >= 3, `contrast ${theme}: ${fg} on ${bg}`)
    if (shots) await page.screenshot({ path: `${shots}/sidebar-versions-${theme}.png`, animations: "disabled" })
    await page.close()
  }
  {
    const { page, v } = await open({ locale: "en", colorScheme: "light" })
    assert.equal((await v.innerText()).trim(), "Frontend 4d419d6a · Backend 0.2.53")
    await page.close()
  }
  {
    // not known yet: says so instead of an empty name
    const { page, v } = await open({ locale: "zh-CN" }, "?hub=")
    assert.equal((await v.innerText()).trim(), "客户端 4d419d6a · 服务端 …")
    assert.equal(await v.getAttribute("title"), "客户端 20261105-120301-4d419d6a · 服务端 正在获取…")
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["light/dark", "frontend by commit, backend by version", "links to 设置 · 关于", "en", "backend not known yet"] }))
} finally { await browser.close() }
