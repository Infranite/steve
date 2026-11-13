// Vite-only fixture: 设置 · 外观 · 字号 and 减弱动态效果 apply to the whole app. Choosing 大 grows the type scale (the
// type-* utilities, Tailwind text-*), choosing 减弱动态效果 stops transitions and animations even when the system does
// not ask for less motion; both are saved to the Hub (PATCH /api/settings), come back through the state refresh, and
// are still there after a reload. The Hub here is a stand-in answering /api/state and /api/settings.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

const errors = []
const state = (settings) => ({ workspace: { id: "ws_1", name: "Steve", item_prefix: "ST" }, workspaces: [{ id: "ws_1", name: "Steve", item_prefix: "ST" }], items: [], bots: [], allBots: [], projects: [], sessions: [], decisions: [], recent: [], thread: [], tags: [], tagUses: {}, settings, devices: [], runtimes: [], owner: true })
const px = (page, sel, prop = "fontSize") => page.locator(sel).first().evaluate((el, p) => getComputedStyle(el)[p], prop)
const html = (page) => page.evaluate(() => document.documentElement.className)

try {
  for (const [width, theme] of [[1440, "light"], [390, "dark"]]) {
    const tag = `${width}-${theme}`
    /* the Hub's settings: what PATCH /api/settings saved, what GET /api/state returns */
    const hub = { fontSize: "标准", reduceMotion: "关", onboard: "1" }, saved = []
    const ctx = await browser.newContext({ locale: "zh-CN", reducedMotion: "no-preference", colorScheme: theme, viewport: { width, height: 900 } })
    await ctx.addInitScript(() => { localStorage.setItem("steve-lang", "zh-CN"); localStorage.setItem("steve-onboarded", "1") })
    await ctx.route((u) => u.pathname.startsWith("/api/"), async (route) => {
      const r = route.request(), path = new URL(r.url()).pathname
      if (path === "/api/state") return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(state(hub)) })
      if (path === "/api/settings" && r.method() === "PATCH") { const p = r.postDataJSON(); saved.push(p); Object.assign(hub, p); return route.fulfill({ status: 200, contentType: "application/json", body: "{}" }) }
      return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) })
    })
    const page = await ctx.newPage()
    page.on("pageerror", (e) => errors.push(`${tag}: ${e.message}`))
    await page.goto(base + `/test/fixtures/look.html${theme === "dark" ? "?theme=dark" : ""}#/settings/look`)
    await page.locator('[data-switch="reduce-motion"]').waitFor()

    /* standard: the scale as designed, transitions run */
    const seg = await px(page, '[data-seg="大"]')
    assert.equal(await px(page, "[data-sample-body]"), "14px", `${tag}: type-body at 标准`)
    assert.equal(await px(page, "[data-sample-body]", "lineHeight"), "22px")
    assert.equal(await px(page, "[data-sample-sm]"), "14px", `${tag}: text-sm at 标准`)
    assert.equal(await px(page, "[data-sample-button]", "transitionDuration"), "0.12s", `${tag}: buttons ease their colours`)
    assert.doesNotMatch(await html(page), /font-large|reduce-motion/)

    /* 大: saved to the Hub, and the type scale grows with it */
    await page.locator('[data-seg="大"]').click()
    await page.waitForFunction(() => document.documentElement.classList.contains("font-large"))
    assert.deepEqual(saved.at(-1), { fontSize: "大" })
    assert.equal(await px(page, "[data-sample-body]"), "15.75px", `${tag}: type-body at 大`)
    assert.equal(await px(page, "[data-sample-body]", "lineHeight"), "24.75px")
    assert.equal(await px(page, "[data-sample-sm]"), "15.75px", `${tag}: text-sm at 大`)
    assert.equal(parseFloat(await px(page, '[data-seg="大"]')), parseFloat(seg) * 1.125, `${tag}: an affectUI control's own size grows too`)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${tag}: the page widened at 大`)
    if (shots) await page.screenshot({ path: `${shots}/look-large-${tag}.png`, fullPage: true })

    /* 减弱动态效果: transitions and animations stop although the system does not ask for it */
    await page.locator('[data-switch="reduce-motion"]').click()
    await page.waitForFunction(() => document.documentElement.classList.contains("reduce-motion"))
    assert.deepEqual(saved.at(-1), { reduceMotion: "开" })
    assert.equal(await px(page, "[data-sample-button]", "transitionDuration"), "0.001s", `${tag}: the transition is gone`)
    assert.equal(await page.evaluate(async () => (await import("/src/lib/motion.ts")).reducedMotion()), true, "script animations ask the same question")

    /* a reload: the device paints with what it saw last, and the Hub agrees */
    await page.reload()
    await page.locator('[data-switch="reduce-motion"]').waitFor()
    assert.match(await html(page), /font-large/)
    assert.match(await html(page), /reduce-motion/)
    assert.equal(await px(page, "[data-sample-body]"), "15.75px", `${tag}: 大 survives a reload`)
    assert.equal(await page.locator('[data-seg="大"]').evaluate((el) => el.getAttribute("aria-checked") ?? el.getAttribute("aria-selected")), "true", `${tag}: the control shows 大`)

    /* the Hub is the truth: settings changed elsewhere win over this device's memory */
    Object.assign(hub, { fontSize: "标准", reduceMotion: "关" })
    await page.reload()
    await page.waitForFunction(() => !document.documentElement.classList.contains("font-large") && !document.documentElement.classList.contains("reduce-motion"))
    assert.equal(await px(page, "[data-sample-body]"), "14px")
    assert.equal(await px(page, "[data-sample-button]", "transitionDuration"), "0.12s")
    assert.deepEqual(JSON.parse(await page.evaluate(() => localStorage.getItem("steve-look"))), { fontSize: "标准", reduceMotion: "关" })
    await ctx.close()
  }
  assert.deepEqual(errors, [])
  console.log("look settings ok")
} finally {
  await browser.close()
}
