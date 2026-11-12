// Vite-only fixture (ST-40): the stats page without the top YOLO bar. All /api calls are answered here.
// Checks that the old bar's facts and doors live on the page, the overview figures agree with their drill-down
// lists and the charts, every chart names its unit and range, and nothing overflows on a phone.
// ST-43: “今天” goes by hour — 24 local hours in the stats zone, the running hour marked, the hours to come empty
// (not 0), the hourly bars add up to the day's totals, and an hour drills to exactly that hour.
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
/* 今天 by hour: it is 14:xx; hours after 14 are null (not yet) */
const TODAY = "2026-11-05", CUR = 14
const hours = Array.from({ length: 24 }, (_, h) => `${TODAY}T${String(h).padStart(2, "0")}:00`)
const hourly = (xs) => hours.map((_, h) => (h > CUR ? null : xs[h] ?? 0))
const hCreated = hourly({ 0: 1, 9: 2, 10: 1, 14: 1 }), hDone = hourly({ 9: 1, 11: 2, 14: 1 })
const todaySum = (a) => sum(a.map((v) => v ?? 0))
const todaySummary = { ...summary, range: { from: TODAY, to: TODAY, tz: "Asia/Shanghai" }, throughput: { ...core.throughput, created: todaySum(hCreated), done: todaySum(hDone) } }
const hourSeries = (metric) => {
  const one = (label, values) => ({ bucket: "hour", buckets: hours, spans: hours.map((_, h) => [h * H, (h + 1) * H]), current: CUR, tz: "Asia/Shanghai", series: [line("total", label, values)] })
  if (metric === "created") return one("新建", hCreated)
  if (metric === "done") return one("完成", hDone)
  /* a time or a rate: null in an hour with nothing to measure, as the Hub has it */
  const measured = (xs) => hours.map((_, h) => xs[h] ?? null)
  if (metric === "lead") return { ...one("中位数", []), series: [line("p50", "中位数", measured({ 9: 2 * H, 11: 5 * H, 14: 3 * H })), line("p85", "p85", measured({ 9: 3 * H, 11: 7 * H, 14: 4 * H }))] }
  if (metric === "hit_rate") return one("命中率", measured({ 9: 0.7, 10: 0.8, 14: 0.6 }))
  return one("合计", hourly({ 1: 20000, 9: 300000, 10: 120000, 13: 50000, 14: 80000 }).map((v) => v && (metric === "cost" ? v / 400000 : v)))
}
const itemRow = (no, status) => ({ item_id: `ST-${no}`, no, title: `完成的事项 ${no}`, status, bot_id: "dev", project_id: "p1", created: Date.now() - no * H, done_at: Date.now() - H, lead_ms: no * H, cycle_ms: H, wait_ms: H / 2, agent_ms: H, turns: 4, reworks: 0, first_pass: true, tokens: 100000 * no, est: no * 0.4, unpriced_tokens: 0, reported: 0 })
const usageRow = (key, label) => ({ ...usage, key, label, priced: true })

/* the event explorer: 96 buckets over the page's range, 2 events per bucket, one turn failing in bucket 90; the list is the window */
const eventsFixture = (p) => {
  const lo = p.lo ? Number(p.lo) : Date.parse(days[0] + "T00:00:00+08:00"), hi = p.hi ? Number(p.hi) : Date.parse(days.at(-1) + "T00:00:00+08:00") + 86400000
  const n = 96, step = Math.ceil((hi - lo) / n), a = p.a ? Number(p.a) : lo, b = p.b ? Number(p.b) : hi
  const evs = Array.from({ length: n * 2 }, (_, i) => ({ id: "e" + i, ts: lo + Math.floor(i / 2) * step + (i % 2) * 1000, kind: i % 2 ? "decision" : "turn", what: i === 180 ? "error" : i % 2 ? "accept" : "end_turn", to: i % 2 ? "resolved" : null, bad: i === 180, open: false, item_id: "ST-1", no: 1, title: "一件事", session_id: "s1", dur_ms: 60000, tools: 3, tool_errors: 0, tokens: 1000, by: i % 2 ? "me" : null }))
  const histogram = { start: lo, step, turn: Array(n).fill(1), decision: Array(n).fill(1), mark: Array(n).fill(0), bad: Array.from({ length: n }, (_, i) => (i === 90 ? 1 : 0)) }
  const rows = evs.filter((e) => e.ts >= a && e.ts < b).sort((x, y) => y.ts - x.ts)
  const counts = { turn: rows.filter((e) => e.kind === "turn").length, decision: rows.filter((e) => e.kind === "decision").length, mark: 0, bad: rows.filter((e) => e.bad).length }
  return { lo, hi, a, b, histogram, counts, rows: rows.slice(0, 200), total: rows.length, capped: false }
}

const errors = [], calls = []
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", (e) => errors.push(e.message))
    const c0 = calls.length
    await page.route((u) => u.pathname.startsWith("/api/") || u.pathname.startsWith("/healthz"), async (route) => {
      const u = new URL(route.request().url()), p = Object.fromEntries(u.searchParams)
      calls.push(u.pathname + u.search)
      let data
      if (u.pathname === "/api/stats/summary") data = p.range === "today" ? todaySummary : summary
      else if (u.pathname === "/api/stats/series") data = p.bucket === "hour" ? hourSeries(p.metric) : series(p.metric, p.group)
      else if (u.pathname === "/api/stats/breakdown") data = { by: p.by, rows: p.by === "decision_kind" ? [{ key: "accept", label: "验收", count: 8, response: P(H / 3, H) }, { key: "approve", label: "审批", count: 5, response: P(H / 6, H / 2) }, { key: "ask", label: "问题", count: 1, response: P(H, H) }]
        : p.by === "tool" ? [{ key: "execute", label: "执行命令", count: 500, dim: "kind" }, { key: "edit", label: "改文件", count: 300, dim: "kind" }, { key: "read", label: "读文件", count: 100, dim: "kind" }, { key: "Bash", label: "Bash", count: 500, dim: "tool" }]
        : [usageRow("agent", "Agent"), usageRow("gateway", "网关")] }
      else if (u.pathname === "/api/stats/items") {
        const rows = p.hour != null ? Array.from({ length: (p.only === "done" ? hDone : hCreated)[Number(p.hour)] ?? 0 }, (_, i) => itemRow(40 + i, p.only === "done" ? "done" : "run"))
          : p.only === "done" && p.range === "custom" ? [itemRow(21, "done")] : p.only === "done" ? Array.from({ length: sum(done) }, (_, i) => (i === 0 ? { ...itemRow(20, "done"), tokens: 7_400_000, est: 0.004, unpriced_tokens: 7_390_000 } : i === 1 ? { ...itemRow(21, "done"), unpriced_tokens: 50_000 } : itemRow(20 + i, "done"))) : p.only === "created" ? [itemRow(30, "run"), itemRow(31, "wait")] : [itemRow(1, "run"), itemRow(8, "done")]
        data = { rows, total: rows.length, next: null }
      } else if (u.pathname === "/api/stats/events") data = eventsFixture(p)
      else return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) })
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
    // millions of tokens on an unpriced model are not “<$0.01”: the cost is unknown, the priced part a lower bound
    const cost = (id) => page.locator(`[data-drill="done"] [data-stats-item="${id}"] td`).last().innerText()
    assert.equal(await cost("ST-20"), "— · 7.4M 未定价")
    assert.equal(await cost("ST-21"), "≥$8.40 · 50K 未定价")
    assert.equal(await cost("ST-22"), "$8.80")
    await page.locator("[data-drill-close]").click()
    assert.equal(await page.locator("[data-drill]").count(), 0)

    /* charts: unit + range on each, legends agree with the summary */
    const range = "10月30日 – 11月5日"
    for (const k of ["flow", "lead", "usage"]) assert.match(await page.locator(`[data-chart-head="${k}"] [data-chart-meta]`).innerText(), new RegExp(range), k)
    assert.match(await page.locator('[data-chart-head="flow"]').innerText(), new RegExp(`新建 ${sum(created)} 件[\\s\\S]*完成 ${sum(done)} 件[\\s\\S]*件 / 天`))
    assert.equal(await page.locator('[data-chart="group"] [data-bar]').count(), days.length)
    // the event explorer: a window dragged on the histogram narrows the list, the counts and the label agree, zoom fills the window
    if (width === 1440) {
      const ev = page.locator("[data-events]")
      await ev.locator("[data-brush] svg").scrollIntoViewIfNeeded()
      assert.equal(await ev.locator('[data-stat="turn"]').innerText().then((x) => x.replace(/\s+/g, " ")), "回合 96")
      assert.equal(await ev.locator("[data-bad]").count(), 1, "the failing bucket is marked")
      // the axis is cut at local midnights (whole days over a week), and the range's exclusive end gets no tick of its own
      const ticks = await ev.locator("[data-tick]").evaluateAll((xs) => xs.map((x) => { const ts = Number(x.getAttribute("data-tick")), d = new Date(ts); return { ts, midnight: d.getHours() === 0 && d.getMinutes() === 0, label: x.textContent } }))
      assert.ok(ticks.length >= 4 && ticks.every((x) => x.midnight && /^\d+\/\d+$/.test(x.label)), JSON.stringify(ticks))
      assert.ok(ticks.every((x) => x.ts < Date.parse(days.at(-1) + "T00:00:00+08:00") + 86400000), "no tick at the end of the range")
      const bb = await ev.locator("[data-brush] svg").boundingBox()
      await page.mouse.move(bb.x + bb.width * 0.5, bb.y + 120); await page.mouse.down()
      await page.mouse.move(bb.x + bb.width * 0.75, bb.y + 120, { steps: 6 }); await page.mouse.up()
      await ev.locator("[data-events-window]").waitFor()
      await page.waitForFunction(() => document.querySelector('[data-events] [data-stat="turn"]')?.textContent?.includes("24"))
      assert.ok(calls.some((c) => c.startsWith("/api/stats/events") && c.includes("a=") && c.includes("b=")), "the window is asked for")
      assert.match(await ev.locator("[data-brush-label]").innerText(), /48 个事件/)
      assert.equal(await ev.locator("[data-event]").count(), 48)
      await ev.locator("[data-zoom-in]").click()
      await ev.locator('[data-zoom-level="0"]').waitFor()
      assert.equal(await ev.locator("[data-events-window]").count(), 0, "zooming in fills the histogram with the window")
      await ev.locator("[data-zoom-out]").click()
      await page.waitForFunction(() => !document.querySelector('[data-events] [data-zoom-level]'))
    }
    // status distribution matches the overview, and opens the same list
    const bars = Object.fromEntries(await page.locator('[data-hbars="status"] [data-hbar]').evaluateAll((xs) => xs.map((x) => [x.getAttribute("data-hbar"), x.textContent])))
    assert.match(bars.run, /3$/); assert.match(bars.wait, /2$/); assert.match(bars.block, /1$/); assert.match(bars.pause, /1$/)
    await page.locator('[data-hbar="run"]').click()
    assert.equal(await page.locator('[data-drill="status:run"] [data-drill-row]').count(), 3)
    // a day on the flow chart drills to that day's items
    if (width === 1440) {
      await page.locator('[data-chart="group"] svg').scrollIntoViewIfNeeded()
      const box = await page.locator('[data-chart="group"] svg').boundingBox()
      await page.mouse.move(box.x + 48 + (box.width - 60) * (5.5 / 7), box.y + 100)
      await page.locator("[data-chart-tip]").waitFor()
      assert.match(await page.locator("[data-chart-tip]").innerText(), /11月4日[\s\S]*新建\s*1[\s\S]*完成\s*1/)
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
    assert.ok(!calls.slice(c0).some((c) => c.includes("bucket=hour")), "7 天 goes by day")

    /* 今天: by hour (ST-43) */
    await page.locator("[data-drill-close]").click().catch(() => {})
    await page.locator('[data-stats-toolbar] [data-seg="today"]').click()
    await page.locator('[data-chart="group"] [data-current]').waitFor()
    await page.getByText(/今天 · 11月5日 · 按小时，北京时间/).first().waitFor()
    const flow = page.locator('[data-chart-head="flow"]')
    assert.match(await flow.locator("[data-chart-meta]").innerText(), /件 \/ 小时 · 11月5日 00:00–24:00 · 北京时间/)
    for (const k of ["lead", "usage"]) assert.match(await page.locator(`[data-chart-head="${k}"] [data-chart-meta]`).innerText(), /小时[\s\S]*11月5日 00:00–24:00 · 北京时间/, k)
    // the overview stays the day's total, and the legend says the same
    assert.match(await kpi("done"), new RegExp(`已完成 · 今天 ${todaySum(hDone)}`))
    assert.match(await flow.innerText(), new RegExp(`新建 ${todaySum(hCreated)} 件[\\s\\S]*完成 ${todaySum(hDone)} 件[\\s\\S]*当前小时，未结束`))
    assert.ok(calls.some((c) => c.includes("metric=created") && c.includes("range=today") && c.includes("bucket=hour")))
    const g = page.locator('[data-chart="group"]').first()
    assert.equal(await g.locator("[data-bar]").count(), 24)
    assert.equal(await g.locator("[data-current]").getAttribute("data-current"), hours[CUR])
    // the hours to come draw nothing; the axis reads local hours
    for (let h = CUR + 1; h < 24; h++) assert.equal(await g.locator(`[data-bar="${hours[h]}"] path`).count(), 0, `hour ${h} is empty`)
    const ticks = await g.locator("svg > text").evaluateAll((xs) => xs.map((x) => x.textContent))
    assert.equal(ticks[0], "00:00"); assert.ok(ticks.every((t) => /^\d\d:00$/.test(t)), ticks.join(","))
    // the table: every hour, 未到 after now, and the column sums are the day's totals
    await flow.locator("[data-chart-mode]").click()
    const rows = await page.locator('[data-flow-chart] [data-table-row]').evaluateAll((xs) => xs.map((x) => [...x.querySelectorAll("td")].map((td) => td.textContent)))
    assert.equal(rows.length, 24)
    assert.deepEqual(rows[0], ["00:00–01:00", "1", "0"])
    assert.deepEqual(rows[CUR], ["14:00–15:00未结束", "1", "1"])
    assert.deepEqual(rows[23], ["23:00–24:00", "未到", "未到"])
    const col = (i) => sum(rows.map((r) => Number(r[i]) || 0))
    assert.deepEqual([col(1), col(2)], [todaySum(hCreated), todaySum(hDone)])
    await flow.locator("[data-chart-mode]").click()
    // the chart is back: wait until it has its width again before aiming at an hour
    await page.waitForFunction(() => { const c = document.querySelector('[data-flow-chart] [data-chart="group"]'), v = c?.querySelector("svg"); return !!v && Math.round(v.getBoundingClientRect().width) === c.clientWidth && Number(v.getAttribute("width")) === c.clientWidth })
    if (width === 1440) {
      // measured each time: the drill panel above moves the chart down
      const at = async (h) => { await g.locator("svg").scrollIntoViewIfNeeded(); const box = await g.locator("svg").boundingBox(); return [box.x + 48 + (box.width - 60) * ((h + 0.5) / 24), box.y + 100] }
      await page.mouse.move(...(await at(20)))
      await page.locator("[data-chart-tip] [data-tip-future]").waitFor()
      assert.match(await page.locator("[data-chart-tip]").innerText(), /11月5日 20:00–21:00[\s\S]*还没到这个小时/)
      await page.mouse.move(...(await at(CUR)))
      await page.locator("[data-chart-tip] [data-tip-current]").waitFor()
      assert.match(await page.locator("[data-chart-tip]").innerText(), /14:00–15:00[\s\S]*未结束，截至[\s\S]*新建\s*1[\s\S]*完成\s*1/)
      // an hour drills to that hour's items, the same count as its bar
      await page.mouse.move(...(await at(9)))
      assert.match(await page.locator("[data-chart-tip]").innerText(), /11月5日 09:00–10:00[\s\S]*新建\s*2[\s\S]*完成\s*1[\s\S]*点击看这个小时的明细/)
      await page.mouse.click(...(await at(9)))
      const drill = page.locator(`[data-drill="day:${hours[9]}"]`)
      await drill.locator("[data-items-total]").waitFor()
      assert.match(await drill.locator("h3").innerText(), /11月5日 09:00–10:00 的事项/)
      assert.equal(Number(await drill.locator("[data-items-total]").getAttribute("data-items-total")), hDone[9])
      assert.ok(calls.some((c) => c.startsWith("/api/stats/items") && c.includes("only=done") && c.includes("from=2026-11-05") && c.includes("to=2026-11-05") && c.includes("hour=9")))
      await drill.locator('[data-seg="created"]').click()
      await page.waitForFunction((n) => document.querySelector('[data-drill] [data-items-total]')?.getAttribute("data-items-total") === String(n), hCreated[9])
      // a future hour can't be picked
      await page.mouse.move(...(await at(20)))
      await page.locator("[data-chart-tip] [data-tip-future]").waitFor()
      await page.mouse.click(...(await at(20)))
      assert.equal(await page.locator(`[data-drill="day:${hours[20]}"]`).count(), 0)
      // the usage trend: pick an hour, the usage table follows it
      await page.locator('[data-trend] [data-chart] svg').scrollIntoViewIfNeeded()
      await page.waitForFunction(() => { const c = document.querySelector("[data-trend] [data-chart]"), v = c?.querySelector("svg"); return !!v && Math.round(v.getBoundingClientRect().width) === c.clientWidth && Number(v.getAttribute("width")) === c.clientWidth })
      await page.locator('[data-trend] [data-chart] svg').scrollIntoViewIfNeeded()
      const ub = await page.locator('[data-trend] [data-chart] svg').boundingBox()
      const asked = page.waitForRequest((r) => r.url().includes("/api/stats/breakdown") && r.url().includes("hour=9") && r.url().includes("from=2026-11-05"))
      await page.mouse.move(ub.x + 48 + (ub.width - 60) * (9.5 / 24), ub.y + 100)
      await page.locator("[data-trend] [data-chart-tip]").waitFor()
      await page.mouse.click(ub.x + 48 + (ub.width - 60) * (9.5 / 24), ub.y + 100)
      await asked
      await page.locator("[data-picked]").waitFor()
      assert.match(await page.locator("[data-picked]").innerText(), /11月5日 09:00–10:00 · 看全天/)
      assert.match(await page.locator('[data-chart-head="usage-table"]').innerText(), /只看 11月5日 09:00–10:00/)
      await page.locator("[data-picked]").click()
    }
    const past = await page.evaluate(() => [...document.querySelectorAll("[data-chart] svg, [data-chart-head], [data-chart-head] *")].filter((e) => e.getBoundingClientRect().right > innerWidth + 1).map((e) => `${Math.round(e.getBoundingClientRect().right)} ${e.outerHTML.slice(0, 160)}`))
    assert.deepEqual(past, [], "by hour: nothing past the right edge")
    if (shots) {
      await page.locator("[data-drill-close]").click().catch(() => {})
      await page.mouse.move(0, 0)
      await page.locator("[data-overview]").evaluate((el) => el.scrollIntoView())
      await page.locator('[data-chart-head="flow"]').evaluate((el) => el.scrollIntoView({ block: "start" }))
      await page.screenshot({ path: `${shots}/stats-today-${width}-${theme}.png`, animations: "disabled" })
    }
    await page.locator('[data-stats-toolbar] [data-seg="7d"]').click()
    await page.locator('[data-chart="group"] [data-bar]').nth(days.length - 1).waitFor()
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
          await page.screenshot({ path: `${shots}/stats-${width}-${theme}-full.png`, animations: "disabled" })
      }
    }
    await page.close()
  }

  /* paused: the breaker's alert with 恢复 */
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 390, height: 844 } })
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
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "运行状态 replaces the top bar (counts, 记录, 调整)", "core figures = drill lists", "flow legend = summary; day drill", "status bars = overview", "unit + range on every chart", "model trend folds into 其他", "paused alert with 恢复", "no horizontal overflow", "event explorer: drag a window, list = window, zoom in and out", "今天 by hour: 24 local hours, current marked, future empty, table sums = day totals, hour drill (items + usage)"] }))
} finally { await browser.close() }
