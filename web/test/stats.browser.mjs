// Vite-only fixture (ST-40): the stats page without the top YOLO bar. All /api calls are answered here.
// Checks that the old bar's facts and doors live on the page, the overview figures agree with their drill-down
// lists and the charts, every chart names its unit and range, and nothing overflows on a phone.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

const days = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02", "2026-11-03", "2026-11-04", "2026-11-05"]
const created = [2, 1, 0, 3, 1, 1, 1], done = [1, 0, 1, 2, 0, 1, 1]
const sum = (a) => a.reduce((x, y) => x + y, 0)
const P = (p50, p85, n = 6) => ({ p50, p85, avg: p50 && p50 * 1.2, sum: (p50 ?? 0) * n, n })
const H = 3600000
const usage = { input: 1_200_000, output: 300_000, cache_read: 4_100_000, cache_write: 400_000, reasoning: 0, total: 6_000_000, hit_rate: 0.72, est: 18.4, reported: 0, unpriced_tokens: 0, saved_by_cache: 9.1, calls: 40, failed_calls: 1, turns: 120, turn_ms: 0, turn_errors: 2, retries: 1, interrupted: 0, tools: 900, tool_errors: 12 }
const core = {
  usage,
  spend: { est: 18.4, reported: 0, unpriced_tokens: 500, unpriced: { "mystery-1": 500 }, saved_by_cache: 9.1, by_source: { agent: 16, gateway: 2.4 }, gateway_by_purpose: {}, priced_by: { table: 4_000_000, models_dev: 1_999_500 }, catalog: { synced: Date.now() - 3600000, tried: Date.now() - 600000, error: "连接 Models.dev 超时", models: 8536 } },
  throughput: { created: sum(created), created_by_source: {}, done: sum(done), dropped: 1, drop_rate: 0.14, wip: 7, delivered: 7, delivered_auto: 2, delivered_manual: 5, net: 2 },
  time: { lead: P(5 * H, 20 * H), cycle: P(3 * H, 9 * H), first_deliver: P(2 * H, 6 * H), wait: P(1.5 * H, 8 * H), agent: P(2 * H, 5 * H), other: P(1 * H, 3 * H), releases: 4, released: 3, release_success_rate: 0.75, not_released: 0, accept_to_release: P(H / 4, H) },
  quality: { first_pass: 0.66, first_pass_n: 6, reworks: 2, reworks_per_done: 0.33, fails: { stall: 1, error: 2, restart: 0, release: 1, other: 0 }, stalled_then_done: 0, rollbacks: 0, release_unknown: 0, turn_error_rate: 0.017, reopened: 0 },
  human: { decisions: 14, by_kind: {}, response: P(H / 3, 2 * H), response_by_kind: {}, expired: 1, yolo: { decided: 3, declined: 1, undone: 0, rate: 0.75 }, messages: 9, per_done: 2.3 },
  agents: { turns: 120, turn_ms: P(4 * 60000, 12 * 60000), tools: 900, tool_error_rate: 0.013, tool_kinds: {}, retries: 1, retry_success: 1, gateway_fallbacks: 0, gateway_failed: 0, permissions: 5, interrupted: 0, cancelled: 1, tokens_per_turn: 50000, tools_per_turn: 7.5 },
  per_item: { tokens: P(800000, 2_000_000), est_p50: 2, est_avg: 2.6, est_top: [] },
  context_over_80: 0, since: { tokens: "2026-10-20", gateway: "2026-10-20" },
}
const summary = { range: { from: days[0], to: days.at(-1), tz: "Asia/Shanghai" }, ...core, prev: { ...core, throughput: { ...core.throughput, done: 4 } } }
const line = (key, label, values) => ({ key, label, values })
const series = (metric, group) => {
  if (metric === "created") return { buckets: days, series: [line("total", "新建", created)] }
  if (metric === "done") return { buckets: days, series: [line("total", "完成", done)] }
  if (metric === "lead") return { buckets: days, series: [line("p50", "中位数", [4, null, 6, 3, null, 8, 5].map((v) => v && v * H)), line("p85", "p85", [9, null, 10, 7, null, 20, 9].map((v) => v && v * H))] }
  if (metric === "hit_rate") return { buckets: days, series: [line("total", "命中率", [0.6, 0.7, 0.75, 0.71, 0.8, 0.68, 0.72])] }
  const scale = metric === "cost" ? 1 / 400000 : 1
  if (group === "token_type") return { buckets: days, series: ["input", "output", "cache_read", "cache_write"].map((k, i) => line(k, k, days.map((_, d) => (i + 1) * 100000 * (d + 2) * scale))) }
  if (group === "model") return { buckets: days, series: ["opus", "sonnet", "haiku", "gpt", "kimi", "qwen", "glm"].map((k, i) => line(k, k, days.map((_, d) => (7 - i) * 50000 * (d + 1) * scale))) }
  return { buckets: days, series: [line("total", "合计", days.map((_, d) => 800000 * (d + 1) * scale))] }
}
const itemRow = (no, status) => ({ item_id: `ST-${no}`, no, title: `完成的事项 ${no}`, status, bot_id: "dev", project_id: "p1", created: Date.now() - no * H, done_at: Date.now() - H, lead_ms: no * H, cycle_ms: H, wait_ms: H / 2, agent_ms: H, turns: 4, reworks: 0, first_pass: true, tokens: 100000 * no, est: no * 0.4, reported: 0 })
const usageRow = (key, label) => ({ ...usage, key, label, priced: true })

const errors = [], calls = []
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route((u) => u.pathname.startsWith("/api/") || u.pathname.startsWith("/healthz"), async (route) => {
      const u = new URL(route.request().url()), p = Object.fromEntries(u.searchParams)
      calls.push(u.pathname + u.search)
      let data
      if (u.pathname === "/api/stats/summary") data = summary
      else if (u.pathname === "/api/stats/series") data = series(p.metric, p.group)
      else if (u.pathname === "/api/stats/breakdown") data = { by: p.by, rows: p.by === "decision_kind" ? [{ key: "accept", label: "验收", count: 8, response: P(H / 3, H) }, { key: "approve", label: "审批", count: 5, response: P(H / 6, H / 2) }, { key: "ask", label: "问题", count: 1, response: P(H, H) }]
        : p.by === "tool" ? [{ key: "execute", label: "执行命令", count: 500, dim: "kind" }, { key: "edit", label: "改文件", count: 300, dim: "kind" }, { key: "read", label: "读文件", count: 100, dim: "kind" }, { key: "Bash", label: "Bash", count: 500, dim: "tool" }]
        : [usageRow("agent", "Agent"), usageRow("gateway", "网关")] }
      else if (u.pathname === "/api/stats/items") {
        const rows = p.only === "done" && p.range === "custom" ? [itemRow(21, "done")] : p.only === "done" ? Array.from({ length: sum(done) }, (_, i) => itemRow(20 + i, "done")) : p.only === "created" ? [itemRow(30, "run"), itemRow(31, "wait")] : [itemRow(1, "run"), itemRow(8, "done")]
        data = { rows, total: rows.length, next: null }
      } else return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) })
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(data) })
    })
    await page.goto(base + `/test/fixtures/stats.html${theme === "dark" ? "?theme=dark" : ""}`)
    await page.locator("[data-stats]").waitFor()
    await page.locator('[data-chart="group"]').waitFor()

    /* the old bar's facts and doors */
    const run = page.locator("[data-run-state]")
    assert.equal(await run.getAttribute("data-run-state"), "full")
    assert.match(await run.innerText(), /完全放开[\s\S]*Agent 不再请求权限[\s\S]*这次替你做了\s*3[\s\S]*留给你\s*1/)
    assert.equal(await run.locator("[data-run-digest]").getAttribute("href"), "#/yolo")
    await run.locator("[data-run-adjust]").click()
    assert.equal(await page.locator("output[data-sheet]").getAttribute("data-sheet"), "yolo")

    /* core figures: now from the item list, the range from the summary; every one opens its list */
    const kpi = async (k) => (await page.locator(`[data-kpi="${k}"]`).innerText()).replace(/\s+/g, " ")
    assert.match(await kpi("run"), /进行中 · 现在 3/)
    assert.match(await kpi("wait"), /待你处理 · 现在 2/)
    assert.match(await kpi("done"), new RegExp(`已完成 · 近 7 天 ${sum(done)} 比上个 7 天 ↑ 50%`))
    assert.match(await kpi("fail"), /失败或受阻 · 现在 2 异常卡 1 · 等前置 1/)
    for (const [k, n] of [["run", 3], ["wait", 2], ["fail", 2]]) {
      await page.locator(`[data-kpi="${k}"]`).click()
      assert.equal(await page.locator(`[data-drill="${k}"] [data-drill-row]`).count(), n, `drill ${k}`)
    }
    await page.locator('[data-kpi="done"]').click()
    await page.locator('[data-drill="done"] [data-items-total]').waitFor()
    assert.equal(Number(await page.locator('[data-drill="done"] [data-items-total]').getAttribute("data-items-total")), sum(done))
    assert.ok(calls.some((c) => c.startsWith("/api/stats/items") && c.includes("only=done") && c.includes("sort=done_at")))
    await page.locator("[data-drill-close]").click()
    assert.equal(await page.locator("[data-drill]").count(), 0)

    /* charts: unit + range on each, legends agree with the summary */
    const range = "10月30日 – 11月5日"
    for (const k of ["flow", "lead", "usage"]) assert.match(await page.locator(`[data-chart-head="${k}"] [data-chart-meta]`).innerText(), new RegExp(range), k)
    assert.match(await page.locator('[data-chart-head="flow"]').innerText(), new RegExp(`件 / 天[\\s\\S]*新建 ${sum(created)} 件[\\s\\S]*完成 ${sum(done)} 件`))
    assert.equal(await page.locator('[data-chart="group"] [data-bar]').count(), days.length)
    // status distribution matches the overview, and opens the same list
    const bars = Object.fromEntries(await page.locator('[data-hbars="status"] [data-hbar]').evaluateAll((xs) => xs.map((x) => [x.getAttribute("data-hbar"), x.textContent])))
    assert.match(bars.run, /3$/); assert.match(bars.wait, /2$/); assert.match(bars.block, /1$/); assert.match(bars.pause, /1$/)
    await page.locator('[data-hbar="run"]').click()
    assert.equal(await page.locator('[data-drill="status:run"] [data-drill-row]').count(), 3)
    // a day on the flow chart drills to that day's items
    if (width === 1440) {
      const box = await page.locator('[data-chart="group"] svg').boundingBox()
      await page.mouse.move(box.x + 48 + (box.width - 60) * (5.5 / 7), box.y + 100)
      await page.locator("[data-chart-tip]").waitFor()
      assert.match(await page.locator("[data-chart-tip]").innerText(), /2026-11-04[\s\S]*新建\s*1[\s\S]*完成\s*1/)
      await page.mouse.click(box.x + 48 + (box.width - 60) * (5.5 / 7), box.y + 100)
      await page.locator('[data-drill="day:2026-11-04"] [data-items-total]').waitFor()
      assert.ok(calls.some((c) => c.includes("only=done") && c.includes("from=2026-11-04") && c.includes("to=2026-11-04")))
      // the model trend folds the 6th group on into 其他
      await page.locator('[data-testid="trend-group"], [data-select="trend-group"]').first().click()
      await page.getByRole("option", { name: "按模型" }).click()
      await page.waitForFunction(() => document.querySelector('[data-chart-head="usage"] [data-legend]')?.textContent?.includes("其他 3 项"))
    }
    // spend: unpriced models make it incomplete (not free), and the unit prices name their source
    const note = (await page.locator("[data-price-note]").innerText()).replace(/\s+/g, " ")
    assert.match(note, /费用未完整估算：1 个模型没有价格（mystery-1），500 tokens 未计入/)
    assert.match(note, /单价来自价格表和 Models\.dev（\d+月\d+日 \d\d:\d\d 同步），最近一次同步失败，用的是上次的价格。管理价格/)
    assert.equal(await page.locator("[data-price-note] a").getAttribute("href"), "#/settings/stats")
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
    // nothing on the page runs past the right edge either (the scroller hides it, so measure the pieces)
    const wide = await page.evaluate(() => [...document.querySelectorAll("[data-chart] svg, [data-hbar], [data-kpi], [data-chart-head], [data-run-state]")].filter((e) => e.getBoundingClientRect().right > innerWidth + 1).map((e) => e.outerHTML.slice(0, 80)))
    assert.deepEqual(wide, [], "pieces past the right edge")
    if (shots) {
      await page.locator("[data-drill-close]").click().catch(() => {})
      await page.mouse.move(0, 0)
      const scroller = page.locator("[data-stats]")
      await scroller.evaluate((el) => el.closest(".overflow-y-auto, [data-scroll], main")?.scrollTo?.(0, 0))
      await page.screenshot({ path: `${shots}/stats-${width}-${theme}.png`, fullPage: true, animations: "disabled" })
      const h = await page.evaluate(() => { const el = document.querySelector("[data-stats]"); let p = el; while (p && !(p.scrollHeight > p.clientHeight + 4 && getComputedStyle(p).overflowY !== "visible")) p = p.parentElement; return p ? p.scrollHeight : 0 })
      if (h) {
        await page.setViewportSize({ width, height: Math.min(h + 120, 6000) })
        await page.evaluate(() => document.querySelectorAll("*").forEach((e) => { e.scrollTop = 0 }))
        await page.waitForTimeout(300)
        await page.screenshot({ path: `${shots}/stats-${width}-${theme}-full.png`, animations: "disabled" })
      }
    }
    await page.close()
  }

  /* paused: the breaker's alert with 恢复 */
  const page = await browser.newPage({ viewport: { width: 390, height: 844 } })
  await page.route((u) => u.pathname.startsWith("/api/"), (route) => route.fulfill({ contentType: "application/json", body: JSON.stringify(route.request().url().includes("summary") ? summary : { buckets: days, series: [], rows: [], total: 0, next: null }) }))
  await page.goto(base + "/test/fixtures/stats.html?yolo=paused")
  await page.locator('[data-run-state="paused"]').waitFor()
  assert.equal(await page.locator('[data-run-state="paused"]').getAttribute("role"), "alert")
  assert.match(await page.locator("[data-run-state]").innerText(), /YOLO 已暂停[\s\S]*1 小时内撤销了 2 次[\s\S]*恢复/)
  // and on the inbox, where the cards it stopped deciding now wait
  await page.goto(base + "/test/fixtures/stats.html?yolo=paused&view=inbox")
  await page.locator('[data-run-state="paused"] [data-run-resume]').waitFor()
  assert.equal(await page.locator('[data-yolo-pill="paused"]:visible').count(), 1, "the pill shows on a phone too")
  if (shots) await page.screenshot({ path: `${shots}/inbox-paused-390.png`, animations: "disabled" })
  await page.close()
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "运行状态 replaces the top bar (counts, 记录, 调整)", "core figures = drill lists", "flow legend = summary; day drill", "status bars = overview", "unit + range on every chart", "model trend folds into 其他", "paused alert with 恢复", "no horizontal overflow"] }))
} finally { await browser.close() }
