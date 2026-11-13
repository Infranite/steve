// Vite-only fixture; external Playwright prerequisite, no API calls.
// ST-42: on an item page a session sits below the lines written while it ran; a comment or a new session still splits the feed.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
const expected = [
  "建立事项", "开始会话", "更新了计划", "换了一版计划", "交付", "会话 #1", "验收没通过",
  /* the new request: its reply, then session #2 below what #2 wrote */
  "我:再加一个需求", "回复:会话 #2", "开始会话", "更新了计划", "换了一版计划", "会话 #2",
  /* a comment to the running session: what it wrote since, then the reply still being written */
  "我:顺便看一下手机端", "更新了计划", "回复:会话 #2",
]
try {
  for (const [width, dark] of [[1066, false], [1066, true], [390, false]]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 1100 } })
    page.on("pageerror", e => errors.push(e.message))
    await page.route("**/*", route => {
      const u = new URL(route.request().url())
      if (u.origin !== base || u.pathname.startsWith("/api/")) return route.abort("blockedbyclient")
      return route.continue()
    })
    await page.goto(base + "/test/fixtures/item-feed.html" + (dark ? "?dark" : ""))
    await page.locator('[data-session-block="s2"]').waitFor()
    await page.locator('[data-reply="s2"]').nth(1).waitFor()
    // runs of process lines are folded; open them and read their lines where they stand
    for (const run of await page.locator("[data-process-run] > button").all()) await run.click()
    const feed = await page.locator('[data-session-block]').first().locator("xpath=..").evaluate((el) => [...el.children].flatMap((x) => x.matches("[data-process-run]") ? [...x.lastElementChild.children] : [x]).map((x) => {
      if (x.matches("[data-session-block]")) return x.querySelector("button span")?.textContent ?? ""
      if (x.matches("[data-reply]")) return "回复:" + (x.textContent.match(/会话 #\d/)?.[0] ?? "")
      if (x.matches("[data-comment]")) return "我:" + (x.querySelector(".type-body")?.textContent ?? "")
      return x.textContent ?? ""
    }))
    assert.equal(feed.length, expected.length, JSON.stringify(feed))
    feed.forEach((x, n) => assert.ok(x.includes(expected[n]), `entry ${n}: ${x} should have ${expected[n]}`))
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/item-feed-${width}${dark ? "-dark" : ""}.png`, fullPage: true })
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["session below its lines", "ended session keeps later lines below", "comment and new session split the feed", "live reply below its lines", "1066/390px", "light/dark"] }))
} finally { await browser.close() }
