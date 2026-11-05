// Vite-only fixture; external Playwright prerequisite, no API calls.
// ST-48: on an item page one run shows its progress once. The turn answering a comment lives under the comment;
// the session block points there instead of repeating it, and both time a turn from its prompt.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
const secs = (t) => { const c = /(\d+):(\d\d)/.exec(t); if (c) return +c[1] * 60 + +c[2]; const d = /(?:(\d+) 分 )?(\d+) 秒/.exec(t); return d ? +(d[1] ?? 0) * 60 + +d[2] : NaN }
const shot = async (page, name) => { if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/run-progress-${name}.png`, fullPage: true }) }
async function open(page, mode, t) {
  await page.goto(`${base}/test/fixtures/run-progress.html?t=${t}${mode ? "&" + mode : ""}`)
  await page.locator('[data-session-block="s1"]').waitFor()
  await page.locator('[data-reply="s1"]').waitFor()
}
/* the work blocks, as [state, time, stats] */
const works = (page) => page.locator("[data-work]").evaluateAll((xs) => xs.map((x) => {
  const t = x.querySelector("button")
  return [x.getAttribute("data-work"), t.querySelector(".tabular-nums")?.textContent ?? "", t.querySelector(".truncate")?.textContent?.replace(/^· /, "") ?? ""]
}))
try {
  for (const [width, dark] of [[1066, false], [1066, true], [390, false]]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 1100 } })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", (route) => {
      const u = new URL(route.request().url())
      if (u.origin !== base || u.pathname.startsWith("/api/")) return route.abort("blockedbyclient")
      return route.continue()
    })
    const t = Date.now()
    await open(page, dark ? "dark" : "", t)
    const reply = page.locator('[data-reply="s1"]')
    /* folded: the header counts the work as the work block does, no separate tool count */
    const folded = await reply.locator("[data-reply-stats]").textContent()
    assert.equal(folded, "· 运行 4 条命令 · 改动 1 个文件")
    assert.doesNotMatch(await reply.textContent(), /调用工具/)

    /* open both: the running turn shows once, under the comment */
    await page.locator('[data-session-block="s1"] button[aria-expanded]').click()
    await reply.locator("[data-reply-toggle]").click()
    await page.locator('[data-reply="s1"] [data-work="live"]').waitFor()
    const ws = await works(page)
    assert.equal(ws.filter((w) => w[0] === "live").length, 1, JSON.stringify(ws))
    assert.equal(await page.locator('[data-session-block="s1"] [data-work="live"]').count(), 0)
    assert.equal(await page.locator('[data-session-block="s1"] [data-away]').count(), 1)
    assert.match(await page.locator('[data-session-block="s1"] [data-work="done"]').textContent(), /已处理8 分 0 秒/)
    assert.doesNotMatch(await page.locator('[data-session-block="s1"]').textContent(), /用户批准了/)
    /* the header gives up its summary to the work block: one count, one clock */
    assert.equal(await reply.locator("[data-reply-stats]").count(), 0)
    const live = ws.find((w) => w[0] === "live")
    assert.equal(live[2], "运行 4 条命令 · 改动 1 个文件")
    /* timed from the approval prompt (18 minutes before load), not from the first command after it */
    const want = Math.floor((Date.now() - (t - 18 * 60_000)) / 1000)
    assert.ok(Math.abs(secs(live[1]) - want) <= 2, `live clock ${live[1]} should be about ${want}s`)
    await shot(page, `live-${width}${dark ? "-dark" : ""}`)

    /* folding the work block and the reply survives lines arriving: state is kept per turn */
    await reply.locator('[data-work="live"] > button').click()
    assert.equal(await reply.locator('[data-work="live"] > button').getAttribute("aria-expanded"), "false")

    /* refresh: same turn, same clock source */
    await open(page, dark ? "dark" : "", t)
    await reply.locator("[data-reply-toggle]").click()
    const again = (await works(page)).find((w) => w[0] === "live")
    assert.ok(Math.abs(secs(again[1]) - Math.floor((Date.now() - (t - 18 * 60_000)) / 1000)) <= 2, `after reload ${again[1]}`)
    assert.equal(again[2], live[2])

    /* finished: the clock stops at the turn's end, prompt → end = 14 minutes */
    await open(page, "done" + (dark ? "&dark" : ""), t)
    await page.locator('[data-session-block="s1"] button[aria-expanded]').click()
    await reply.locator("[data-reply-toggle]").click()
    const fin = await works(page)
    assert.equal(fin.filter((w) => w[0] === "live").length, 0)
    const last = await reply.locator('[data-work="done"]').evaluate((x) => x.querySelector("button").textContent)
    assert.match(last, /已处理14 分 0 秒/)
    assert.equal(await page.locator('[data-session-block="s1"] [data-away]').count(), 1)
    await shot(page, `done-${width}${dark ? "-dark" : ""}`)

    /* a permission midway splits the work: the part after it times from its own first step, not again from the prompt */
    await open(page, "perm", t)
    await reply.locator("[data-reply-toggle]").click()
    await reply.locator("[data-permission-record]").waitFor()
    const parts = await works(page)
    assert.equal(parts.length, 2, JSON.stringify(parts))
    assert.deepEqual(parts.map((w) => w[0]), ["done", "live"])
    assert.equal(secs(parts[0][1]), 3 * 60, JSON.stringify(parts))
    assert.ok(Math.abs(secs(parts[1][1]) - Math.floor((Date.now() - (t - 12 * 60_000)) / 1000)) <= 2, JSON.stringify(parts))
    await shot(page, `perm-${width}`)

    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["reply turn shown once", "session block points to the reply", "one stats口径", "clock from the prompt", "fold toggle", "reload", "finished", "permission midway", "1066/390px", "light/dark"] }))
} finally { await browser.close() }
