// Vite-only fixture (ST-52): 设置 · 统计 with the Models.dev prices. All /api calls are answered here.
// Checks that every model used says where its price comes from (price table, Models.dev, unpriced), that a
// failed sync says it keeps the last prices, that "立即同步" syncs and clears it, that "改价" copies a
// Models.dev price into the price table as an override, and that nothing overflows on a phone.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

const H = 3600000, now = Date.now()
const table = [
  { match: "claude-opus-5-5", input: 4, output: 20, cache_read: 0.2, cache_write: 5, note: "官方标价" },
  { match: "claude-sonnet-5-5", input: 2, output: 10, cache_read: 0.2, cache_write: 2.5, note: "官方标价" },
]
const quote = (p, source, x = {}) => ({ ...p, source, via: null, since: 0, ...x })
const prices = (failed) => ({
  prices: table, custom: false, defaults: table,
  unpriced: [{ model: "mystery-1", tokens: 500 }],
  models: [
    { model: "claude-opus-5-5", tokens: 4_000_000, match: "claude-opus-5-5", via: null, price: quote(table[0], "table") },
    { model: "gpt-5", tokens: 2_000_000, match: "openai/gpt-5", via: null, price: quote({ match: "openai/gpt-5", input: 1.25, output: 10, cache_read: 0.125, cache_write: 1.25, note: "Models.dev 没列缓存写入价，按输入价估" }, "models_dev") },
    { model: "kiro/claude-opus-5-5", tokens: 300_000, match: "claude-opus-5-5", via: "kiro/", price: quote(table[0], "table", { via: "kiro/" }) },
    { model: "mystery-1", tokens: 500, match: null, via: null, price: null },
  ],
  catalog: { source: "Models.dev", url: "https://models.dev/api.json", enabled: true, synced: now - 26 * H, tried: failed ? now - H / 2 : now, error: failed ? "连接 Models.dev 超时" : null, models: 8536 },
  tz: null, hub_tz: "Asia/Shanghai",
})

const errors = []
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    let failed = true, synced = 0
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route((u) => u.pathname.startsWith("/api/"), async (route) => {
      const u = new URL(route.request().url())
      let data
      if (u.pathname === "/api/stats/prices") data = prices(failed)
      else if (u.pathname === "/api/stats/prices/sync" && route.request().method() === "POST") { synced++; failed = false; data = { ok: true, models: 8536, changed: 3 } }
      else return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) })
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(data) })
    })
    await page.goto(base + `/test/fixtures/stats-prices.html${theme === "dark" ? "?theme=dark" : ""}`)
    await page.locator("[data-stats-settings]").waitFor()

    /* every model says where its price comes from */
    const src = async (m) => page.locator(`[data-price-model="${m}"]`).getAttribute("data-price-source")
    assert.equal(await src("claude-opus-5-5"), "table")
    assert.equal(await src("gpt-5"), "models_dev")
    assert.equal(await src("kiro/claude-opus-5-5"), "table")
    assert.equal(await src("mystery-1"), "none")
    const gpt = (await page.locator('[data-price-model="gpt-5"]').innerText()).replace(/\s+/g, " ")
    assert.match(gpt, /Models\.dev.*没列缓存写入价.*1\.25 10 0\.125 1\.25/)
    assert.match(await page.locator('[data-price-model="kiro/claude-opus-5-5"]').innerText(), /按 claude-opus-5-5 估/)
    assert.match(await page.locator('[data-price-model="mystery-1"]').innerText(), /未定价/)
    assert.match(await page.locator("[data-unpriced]").innerText(), /不计入估算费用[\s\S]*500 tokens 未计入/)

    /* a failed sync keeps the last prices */
    assert.match(await page.locator("[data-catalog-status]").innerText(), /上次同步 .*，收录 8,536 个模型；每天自动更新/)
    assert.match(await page.locator("[data-catalog-error]").innerText(), /同步失败：连接 Models\.dev 超时。继续使用上次同步的价格/)
    if (shots) await page.screenshot({ path: `${shots}/stats-prices-${width}-${theme}.png`, fullPage: true })

    /* overflow on a phone */
    const over = await page.evaluate(() => [...document.querySelectorAll("[data-stats-settings] section")].filter((s) => s.getBoundingClientRect().right > innerWidth + 1).length)
    assert.equal(over, 0)

    /* the editable price table fits its column on a desktop: its fields take their cells' width */
    if (width === 1440) {
      const [table, room] = await page.locator('[data-price-row="0"]').evaluate((tr) => { const t = tr.closest("table"); return [t.scrollWidth, t.parentElement.clientWidth] })
      assert.ok(table <= room, `the price table is ${table}px wide in a ${room}px column`)
    }

    /* 立即同步 */
    await page.locator("[data-catalog-sync]").click()
    await page.locator("[data-catalog-error]").waitFor({ state: "detached" })
    assert.equal(synced, 1)

    /* 改价: the Models.dev price goes into the price table as an override */
    const rows = await page.locator("[data-price-row]").count()
    await page.locator('[data-price-override="gpt-5"]').click()
    assert.equal(await page.locator("[data-price-row]").count(), rows + 1)
    const last = page.locator(`[data-price-row="${rows}"]`)
    assert.equal(await last.getByLabel(`第 ${rows + 1} 行模型`).inputValue(), "gpt-5")
    assert.equal(await last.getByLabel(`第 ${rows + 1} 行输入价格`).inputValue(), "1.25")
    assert.equal(await last.getByLabel(`第 ${rows + 1} 行缓存读价格`).inputValue(), "0.125")
    assert.match(await last.getByLabel(`第 ${rows + 1} 行备注`).inputValue(), /Models\.dev · openai\/gpt-5/)
    assert.ok(await page.locator('[data-price-override="gpt-5"]').isDisabled(), "already in the table")
    assert.ok(await page.locator("[data-price-save]").isEnabled())
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("stats prices browser: ok")
} finally {
  await browser.close()
}
