// Vite-only fixture: 设置 · 外观 · 配色与字体 (web/src/themes). Picking an accent changes --primary on <html> at once,
// in light and in dark, survives a reload (kept per device in localStorage), and a neutral, a font and the corner
// radius follow the same way; 恢复默认 puts the stylesheet back. A theme registered with a bad token is refused
// without touching what is on screen.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

const errors = []
const css = (page, v) => page.evaluate((n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim(), v)
const open = async (ctx, { width = 1440 } = {}) => {
  const page = await ctx.newPage()
  await page.setViewportSize({ width, height: 900 })
  page.on("pageerror", (e) => errors.push(e.message))
  await page.route((u) => u.pathname.startsWith("/api/"), (route) => route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) }))
  return page
}
const go = async (page, theme) => {
  await page.goto(base + `/test/fixtures/settings-panes.html${theme === "dark" ? "?theme=dark" : ""}#/settings/look`)
  await page.locator("[data-appearance-section]").waitFor()
}

try {
  for (const theme of ["light", "dark"]) {
    const ctx = await browser.newContext({ locale: "zh-CN", reducedMotion: "reduce", colorScheme: theme })
    await ctx.addInitScript(() => localStorage.setItem("steve-lang", "zh-CN"))
    const page = await open(ctx)
    await go(page, theme)
    const before = await css(page, "--primary")
    assert.equal(before, "#3e63dd", `${theme}: indigo is the default`)
    assert.equal(await page.locator('[data-appearance="accent"] [aria-checked="true"]').getAttribute("data-choice"), "indigo")

    /* pick violet: --primary changes now, without a reload */
    await page.locator('[data-appearance="accent"] [data-choice="violet"]').click()
    assert.equal(await css(page, "--primary"), "#6e56cf", `${theme}: violet's primary`)
    assert.equal(await css(page, "--blue"), theme === "dark" ? "#9b8af0" : "#6e56cf", `${theme}: and its blue for this mode`)
    assert.equal(await page.evaluate(() => document.documentElement.dataset.themeAccent), "violet")
    const btn = await page.locator("[data-appearance-preview] [data-slot=button]").last().evaluate((el) => getComputedStyle(el).backgroundColor)
    assert.equal(btn, "rgb(110, 86, 207)", `${theme}: the preview's primary button takes it`)

    /* arrows move the choice as in any radio group */
    await page.locator('[data-appearance="accent"] [data-choice="violet"]').focus()
    await page.keyboard.press("ArrowRight")
    assert.equal(await css(page, "--primary"), "#00786e", `${theme}: → is green`)
    await page.keyboard.press("ArrowLeft")

    /* a neutral, a font and the corners */
    await page.locator('[data-appearance="neutral"] [data-choice="sand"]').click()
    assert.equal(await css(page, "--background"), theme === "dark" ? "#111110" : "#fcfcfb")
    await page.locator('[data-appearance="sans"] [data-seg="system"]').click()
    assert.match(await page.evaluate(() => getComputedStyle(document.body).fontFamily), /^-apple-system/)
    await page.locator('[data-appearance="radius"] [data-seg="rounded"]').click()
    assert.equal(await css(page, "--radius-xl"), "16px")
    const card = await page.locator("[data-appearance-preview] .shadow-\\[var\\(--lift\\)\\]").evaluate((el) => getComputedStyle(el).borderTopLeftRadius)
    assert.equal(card, "16px", "rounded-xl reads the variable")
    if (shots) await page.screenshot({ path: `${shots}/appearance-${theme}.png`, fullPage: true })

    /* a reload keeps all of it */
    await page.reload()
    await page.locator("[data-appearance-section]").waitFor()
    assert.equal(await css(page, "--primary"), "#6e56cf", `${theme}: the accent survives a reload`)
    assert.equal(await css(page, "--radius-xl"), "16px")
    assert.equal(await page.locator('[data-appearance="accent"] [aria-checked="true"]').getAttribute("data-choice"), "violet")
    assert.deepEqual(JSON.parse(await page.evaluate(() => localStorage.getItem("steve-appearance"))), { accent: "violet", neutral: "sand", sans: "system", radius: "rounded" })

    /* a theme with a token outside the whitelist is refused and nothing changes; a good one shows up as a choice */
    const r = await page.evaluate(() => {
      const T = window.steveThemes
      const bad = T.registerTheme({ id: "bad", name: "Bad", kind: "accent", tokens: { light: { primary: "red; } body { display: none" } } })
      const good = T.registerTheme({ id: "plugin.ocean", name: { "zh-CN": "海洋", en: "Ocean" }, kind: "accent", tokens: { light: { primary: "#0b5e8a", "primary-foreground": "#ffffff" }, dark: { primary: "#0b5e8a", "primary-foreground": "#ffffff" } } })
      return { bad: bad.ok, good: good.ok }
    })
    assert.deepEqual(r, { bad: false, good: true })
    assert.equal(await css(page, "--primary"), "#6e56cf", "nothing on screen changed")
    await page.locator('[data-appearance="accent"] [data-choice="plugin.ocean"]').click()
    assert.equal(await css(page, "--primary"), "#0b5e8a", "a registered accent applies like a built-in")
    assert.equal(await page.locator('[data-appearance="accent"] [data-choice="plugin.ocean"]').getAttribute("title"), "海洋")

    /* 恢复默认 */
    await page.locator("[data-appearance-reset]").click()
    assert.equal(await css(page, "--primary"), "#3e63dd")
    assert.equal(await css(page, "--radius-xl"), "12px")
    assert.equal(await page.evaluate(() => localStorage.getItem("steve-appearance")), null)
    assert.equal(await page.evaluate(() => document.getElementById("steve-appearance")?.textContent), "", "the defaults add no CSS")
    await page.locator("[data-appearance-reset]").waitFor({ state: "detached" })

    /* a phone wraps the swatches inside the card */
    const phone = await open(ctx, { width: 390 })
    await go(phone, theme)
    const over = await phone.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)
    assert.ok(over <= 0, `${theme} 390: no sideways scroll (${over})`)
    await ctx.close()
  }
  assert.deepEqual(errors, [])
  console.log("appearance browser test passed")
} finally {
  await browser.close()
}
