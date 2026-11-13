// Vite-only fixture (ST-40, stats v2): the stats page by purpose. All /api calls are answered here.
// Checks the slim 现在 line (with the YOLO run state) and its lists, the key numbers against the range before, the daily
// output chart (done bars + cost line, day drill), the cost / time split and the most expensive items, the sortable Bot
// and harness · model tables (a Bot row filters the page), the folded event explorer with its counts said once,
// and that nothing overflows on a phone.
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
const usage = { input: 1_200_000, output: 300_000, cache_read: 4_100_000, cache_write: 400_000, reasoning: 0, total: 6_000_000, hit_rate: 0.72, est: 18.4, reported: 0, unpriced_tokens: 0, saved_by_cache: 9.1, calls: 40, failed_calls: 1, turns: 120, turn_ms: 26 * 3600000, turn_errors: 2, retries: 1, interrupted: 0, tools: 900, tool_errors: 12 }
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
/* a second Bot: fewer items, each dearer */
const revSummary = { ...summary, throughput: { ...core.throughput, done: 2 }, quality: { ...core.quality, first_pass: 0.5, first_pass_n: 2, reworks: 1, reworks_per_done: 0.5, fails: { stall: 0, error: 0, restart: 0, release: 0, other: 0 } }, per_item: { ...core.per_item, est_avg: 5.5 } }

/* the event explorer: 96 buckets over the page's range, 2 events per bucket, one turn failing in bucket 90; the list is the window */
const eventsFixture = (p) => {
  const lo = p.lo ? Number(p.lo) : Date.parse(days[0] + "T00:00:00+08:00"), hi = p.hi ? Number(p.hi) : Date.parse(days.at(-1) + "T00:00:00+08:00") + 86400000
  const n = 96, step = Math.ceil((hi - lo) / n), a = p.a ? Number(p.a) : lo, b = p.b ? Number(p.b) : hi
  const evs = Array.from({ length: n * 2 }, (_, i) => ({ id: "e" + i, ts: lo + Math.floor(i / 2) * step + (i % 2) * 1000, kind: i % 2 ? "decision" : "turn", what: i === 180 ? "error" : i % 2 ? "accept" : "end_turn", to: i % 2 ? "resolved" : null, bad: i === 180, open: false, item_id: "ST-1", no: 1, title: "一件事", session_id: "s1", dur_ms: 60000, tools: 3, tool_errors: 0, tokens: 1000, by: i % 2 ? "me" : null }))
  const histogram = { start: lo, step, turn: Array(n).fill(1), decision: Array(n).fill(1), mark: Array(n).fill(0), bad: Array.from({ length: n }, (_, i) => (i === 90 ? 1 : 0)) }
  /* a kind narrows the list and the counts to it, as the Hub does */
  const rows = evs.filter((e) => e.ts >= a && e.ts < b && (!p.kind || e.kind === p.kind)).sort((x, y) => y.ts - x.ts)
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
      if (u.pathname === "/api/stats/summary") data = p.range === "today" ? todaySummary : p.bot === "rev" ? revSummary : summary
      else if (u.pathname === "/api/stats/series") data = p.bucket === "hour" ? hourSeries(p.metric) : series(p.metric, p.group)
      else if (u.pathname === "/api/stats/breakdown") data = { by: p.by, rows: p.by === "decision_kind" ? [{ key: "accept", label: "验收", count: 8, response: P(H / 3, H) }, { key: "approve", label: "审批", count: 5, response: P(H / 6, H / 2) }, { key: "ask", label: "问题", count: 1, response: P(H, H) }]
        : p.by === "tool" ? [{ key: "execute", label: "执行命令", count: 500, dim: "kind" }, { key: "edit", label: "改文件", count: 300, dim: "kind" }, { key: "read", label: "读文件", count: 100, dim: "kind" }, { key: "Bash", label: "Bash", count: 500, dim: "tool" }]
        : p.by === "bot" ? [usageRow("dev", "dev"), usageRow("rev", "评审")]
        : p.by === "model" ? [usageRow("opus", "opus"), usageRow("sonnet", "sonnet")]
        : p.by === "harness" ? [usageRow("claude-code", "Claude Code")]
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

    /* the YOLO run state, now on the slim “现在” line */
    const run = page.locator("[data-run-state]")
    assert.equal(await run.getAttribute("data-run-state"), "full")
    assert.match(await run.innerText(), /完全放开[\s\S]*Agent 不再请求权限[\s\S]*这次替你做了\s*3[\s\S]*留给你\s*1/)
    assert.equal(await run.locator("[data-run-digest]").getAttribute("href"), "#/yolo")
    await run.locator("[data-run-adjust]").click()
    assert.equal(await page.locator("output[data-sheet]").getAttribute("data-sheet"), "yolo")

    /* 现在: counts from the item list, each opening its list; it sits above the key numbers, not in their place */
    const kpi = async (k) => (await page.locator(`[data-kpi="${k}"]`).innerText()).replace(/\s+/g, " ").trim()
    // ST-3 runs but a card of it waits for you: its state is yours, not in progress (item-state.ts)
    assert.match(await kpi("run"), /^进行中\s*2$/)
    assert.match(await kpi("wait"), /^等你\s*2$/)
    assert.match(await kpi("fail"), /^失败或受阻\s*2$/)
    for (const [k, n] of [["run", 2], ["wait", 2], ["fail", 2]]) {
      await page.locator(`[data-kpi="${k}"]`).click()
      assert.equal(await page.locator(`[data-drill="${k}"] [data-drill-row]`).count(), n, `drill ${k}`)
    }
    await page.locator("[data-drill-close]").click()
    assert.equal(await page.locator("[data-drill]").count(), 0)
    const nowBox = await page.locator("[data-now]").boundingBox(), keyBox = await page.locator("[data-key-numbers]").boundingBox()
    assert.ok(nowBox.y < keyBox.y && keyBox.y < (width === 390 ? 844 : 1000) - 100, "the key numbers are on the first screen, under the now line")

    /* key numbers: the summary's, each against the range before */
    assert.equal(await page.locator("[data-key-numbers]").getAttribute("data-key-numbers"), "6")
    assert.equal(await kpi("done"), `完成事项 ${sum(done)} 比上个 7 天 ↑ 50%`)
    assert.match(await kpi("est"), /^估算费用\s*估 \$18\.40 比上个 7 天 持平$/)
    assert.match(await kpi("agent"), /^Agent 工作时长 26 小时/)
    assert.match(await kpi("waiting"), /^等你的时间 9 小时/)
    assert.match(await kpi("first_pass"), /一次通过率 66%/)
    assert.match(await kpi("per_item"), /^每件平均费用\s*估 \$2\.60/)

    /* daily output: done per day as bars, the cost as a line on its own axis; a day drills to that day's items */
    const daily = page.locator("[data-daily]")
    assert.match(await daily.locator('[data-chart-head="daily"] [data-chart-meta]').innerText(), /件 \/ 天 · 10月30日 – 11月5日/)
    assert.match(await daily.locator('[data-chart-head="daily"]').innerText(), new RegExp(`完成 ${sum(done)} 件[\\s\\S]*估算费用 \\$18\\.40（右轴）`))
    assert.equal(await daily.locator('[data-chart="group"] [data-bar]').count(), days.length)
    await daily.locator('[data-overlay="cost"] path').waitFor()
    assert.equal(await daily.locator("[data-axis-right]").count(), 3)
    assert.ok(calls.some((c) => c.includes("metric=done")) && calls.some((c) => c.includes("metric=cost")))
    if (width === 1440) {
      await daily.locator('[data-chart="group"] svg').scrollIntoViewIfNeeded()
      const box = await daily.locator('[data-chart="group"] svg').boundingBox()
      const dx = box.x + 48 + (box.width - 104) * (5.5 / 7)
      await page.mouse.move(dx, box.y + 100)
      await page.locator("[data-chart-tip]").waitFor()
      assert.match(await page.locator("[data-chart-tip]").innerText(), /11月4日[\s\S]*完成事项\s*1[\s\S]*估算费用\s*\$/)
      await page.mouse.click(dx, box.y + 100)
      await page.locator('[data-drill="day:2026-11-04"] [data-items-total]').waitFor()
      assert.ok(calls.some((c) => c.includes("only=done") && c.includes("from=2026-11-04") && c.includes("to=2026-11-04")))
      await page.locator("[data-drill-close]").click()
      // the line can be the turns instead
      await daily.locator('[data-seg="turns"]').click()
      await daily.locator('[data-overlay="turns"]').waitFor()
      await daily.locator('[data-seg="cost"]').click()
    }

    /* where the money went: the split (Compact Breakdown) of the range's cost by Bot by default, then by model; the time too */
    const split = page.locator("[data-where-split]")
    await split.scrollIntoViewIfNeeded()
    assert.equal(await split.getAttribute("data-where-split"), "bot")
    assert.deepEqual(await split.locator("[data-where-part]").evaluateAll((els) => els.map((e) => e.getAttribute("data-where-part"))), ["dev", "rev"])
    await page.waitForFunction(() => /\$36\.80/.test(document.querySelector("[data-where-split] .ui-cbc__big")?.textContent ?? ""))
    assert.match((await split.locator('[data-where-part="dev"]').innerText()).replace(/\s+/g, " "), /Steve 开发 Bot \$18\.40 50%/)
    await page.locator('[data-where] [data-seg="model"]').click()
    await page.waitForFunction(() => document.querySelector("[data-where-split]")?.getAttribute("data-where-split") === "model" && document.querySelector('[data-where-part="opus"]'))
    await page.locator('[data-where] [data-seg="time"]').click()
    await page.waitForFunction(() => document.querySelector("[data-where-split]")?.getAttribute("data-where-measure") === "time")
    await page.waitForFunction(() => /小时|天/.test(document.querySelector("[data-where-split] .ui-cbc__big")?.textContent ?? ""))
    await page.locator('[data-where] [data-seg="cost"]').click()
    // the most expensive items, each opening where its cost went
    const top = page.locator("[data-top-item]")
    assert.equal(await top.count(), 2)
    assert.ok(calls.some((c) => c.startsWith("/api/stats/items") && c.includes("sort=est") && c.includes("limit=8")))
    await top.first().click()
    await page.waitForFunction(() => location.hash === "#/stats/items/ST-1")
    // unpriced models make the cost incomplete (not free), and the unit prices name their source
    const note = (await page.locator("[data-price-note]").innerText()).replace(/\s+/g, " ")
    assert.match(note, /费用未完整估算：1 个模型没有价格（mystery-1），500 tokens 未计入/)
    assert.match(note, /单价来自价格表和 Models\.dev（\d+月\d+日 \d\d:\d\d 同步），最近一次同步失败，用的是上次的价格。管理价格/)
    assert.equal(await page.locator("[data-price-note] a").getAttribute("href"), "#/settings/stats")

    /* the Bots compared: one summary per Bot, sortable; a row filters the page by that Bot */
    const bots = page.locator('[data-compare="bots"]')
    await bots.locator("[data-compare-row]").first().waitFor()
    assert.deepEqual(await bots.locator("[data-compare-row]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-compare-row"))), ["dev", "rev"])
    assert.ok(calls.some((c) => c.startsWith("/api/stats/summary") && c.includes("bot=rev")))
    assert.equal((await bots.locator('[data-compare-row="rev"] [data-col="first_pass"]').innerText()).replace(/\s+/g, " "), "50% 2 件有结论")
    assert.match(await bots.locator('[data-compare-row="dev"] [data-col="fails"]').innerText(), /^4\s+每件 0\.7/)
    assert.equal(await bots.locator('[data-compare-row="dev"] [data-col="cost_per_item"]').innerText(), "$2.60")
    await bots.locator('[data-sort-col="done"]').click()
    assert.equal(await bots.getAttribute("data-sorted"), "done:asc")
    assert.deepEqual(await bots.locator("[data-compare-row]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-compare-row"))), ["rev", "dev"])
    await bots.locator('[data-sort-col="cost_per_item"]').click()
    assert.equal(await bots.getAttribute("data-sorted"), "cost_per_item:desc")
    assert.deepEqual(await bots.locator("[data-compare-row]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-compare-row"))), ["rev", "dev"])

    /* harness · model: agent turns only, one row per harness and model */
    const models = page.locator('[data-compare="models"]')
    await models.locator("[data-compare-row]").first().waitFor()
    assert.equal(await models.locator("[data-compare-row]").count(), 2)
    assert.ok(calls.some((c) => c.includes("by=model") && c.includes("source=agent")) && calls.some((c) => c.includes("by=harness") && c.includes("model=opus") && c.includes("source=agent")))
    assert.match((await models.locator("[data-compare-row]").first().locator('[data-col="ok"]').innerText()).replace(/\s+/g, " "), /98\.3% 2 个出错/)
    assert.equal(await models.locator("[data-compare-row]").first().locator('[data-col="cost_per_turn"]').innerText(), "$0.15")

    /* the event explorer: folded at the foot until asked for; then a window dragged on the histogram narrows the list */
    assert.equal(await page.locator("[data-events]").count(), 0, "folded")
    assert.ok(!calls.slice(c0).some((c) => c.startsWith("/api/stats/events")), "nothing loaded while folded")
    await page.locator("[data-events-toggle]").click()
    const ev = page.locator("[data-events]")
    await ev.locator("[data-brush] svg").waitFor()
    if (width === 1440) {
      await ev.locator("[data-brush] svg").scrollIntoViewIfNeeded()
      // the counts are said once: on the kind switch (no strip of figures, no second list)
      assert.equal(await ev.locator("[data-stat-strip]").count(), 0)
      assert.equal(await ev.locator("[data-event-list]").count(), 1)
      assert.equal((await ev.locator('[data-seg="turn"]').innerText()).replace(/\s+/g, " "), "回合 96")
      assert.equal(await ev.locator("[data-bad]").count(), 1, "the failing bucket is marked")
      // the axis is cut at local midnights (whole days over a week), and the range's exclusive end gets no tick of its own
      const ticks = await ev.locator("[data-tick]").evaluateAll((xs) => xs.map((x) => { const ts = Number(x.getAttribute("data-tick")), d = new Date(ts); return { ts, midnight: d.getHours() === 0 && d.getMinutes() === 0, label: x.textContent } }))
      assert.ok(ticks.length >= 4 && ticks.every((x) => x.midnight && /^\d+\/\d+$/.test(x.label)), JSON.stringify(ticks))
      assert.ok(ticks.every((x) => x.ts < Date.parse(days.at(-1) + "T00:00:00+08:00") + 86400000), "no tick at the end of the range")
      const bb = await ev.locator("[data-brush] svg").boundingBox()
      await page.mouse.move(bb.x + bb.width * 0.5, bb.y + 120); await page.mouse.down()
      await page.mouse.move(bb.x + bb.width * 0.75, bb.y + 120, { steps: 6 }); await page.mouse.up()
      await ev.locator("[data-events-window]").waitFor()
      await page.waitForFunction(() => document.querySelector('[data-events] [data-seg="turn"]')?.textContent?.includes("24"))
      assert.ok(calls.some((c) => c.startsWith("/api/stats/events") && c.includes("a=") && c.includes("b=")), "the window is asked for")
      assert.match(await ev.locator("[data-brush-label]").innerText(), /48 个事件/)
      // narrowed to one kind, the other kinds keep their counts
      await ev.locator('[data-seg="turn"]').click()
      await page.waitForFunction(() => document.querySelectorAll("[data-events] [data-event]").length > 0 && [...document.querySelectorAll("[data-events] [data-event]")].every((e) => e.getAttribute("data-event") === "turn"))
      assert.match(await ev.locator('[data-seg="decision"]').innerText(), /决定 24/)
      await ev.locator('[data-seg="all"]').click()
      await page.waitForFunction(() => [...document.querySelectorAll("[data-events] [data-event]")].some((e) => e.getAttribute("data-event") === "decision"))
      // the list shows its first 30 rows in the page (no scroll box inside it), the rest on request
      assert.equal(await ev.locator("[data-event]").count(), 30)
      await ev.locator("[data-events-more]").click()
      assert.equal(await ev.locator("[data-event]").count(), 48)
      await ev.locator("[data-zoom-in]").click()
      await ev.locator('[data-zoom-level="0"]').waitFor()
      assert.equal(await ev.locator("[data-events-window]").count(), 0, "zooming in fills the histogram with the window")
      await ev.locator("[data-zoom-out]").click()
      await page.waitForFunction(() => !document.querySelector('[data-events] [data-zoom-level]'))
    }
    assert.ok(!calls.slice(c0).some((c) => c.includes("bucket=hour")), "7 天 goes by day")
    const wide7 = await page.evaluate(() => [...document.querySelectorAll("[data-chart] svg, [data-kpi], [data-chart-head], [data-run-state], [data-top-item], [data-where-split], [data-compare]")].filter((e) => e.getBoundingClientRect().right > innerWidth + 1).map((e) => e.outerHTML.slice(0, 80)))
    assert.deepEqual(wide7, [], "pieces past the right edge")
    if (shots) {
      await page.mouse.move(0, 0)
      const h = await page.evaluate(() => { const el = document.querySelector("[data-stats]"); let p = el; while (p && !(p.scrollHeight > p.clientHeight + 4 && getComputedStyle(p).overflowY !== "visible")) p = p.parentElement; return p ? p.scrollHeight : 0 })
      if (h) await page.setViewportSize({ width, height: Math.min(h + 120, 8000) })
      await page.evaluate(() => document.querySelectorAll("*").forEach((e) => { e.scrollTop = 0 }))
      await page.screenshot({ path: `${shots}/stats-${width}-${theme}-full.png`, animations: "disabled" })
      await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 })
    }

    /* a row of the Bot table filters the page by that Bot */
    await bots.locator('[data-compare-row="rev"]').click()
    await page.waitForFunction(() => document.querySelector("[data-bots-all]"))
    assert.ok(calls.some((c) => c.startsWith("/api/stats/series") && c.includes("bot=rev")), "the page follows the Bot")
    await page.locator("[data-bots-all]").click()
    await page.waitForFunction(() => !document.querySelector("[data-bots-all]"))

    /* 今天: by hour (ST-43) */
    await page.locator('[data-stats-toolbar] [data-seg="today"]').click()
    await page.locator('[data-daily] [data-chart="group"] [data-current]').waitFor()
    await page.getByText(/今天 · 11月5日 · 按小时，北京时间/).first().waitFor()
    const head = page.locator('[data-chart-head="daily"]')
    assert.match(await head.locator("[data-chart-meta]").innerText(), /件 \/ 小时 · 11月5日 00:00–24:00 · 北京时间/)
    assert.match(await head.innerText(), /每小时完成的事项/)
    // the key numbers stay the day's totals, and the legend says the same
    assert.match(await kpi("done"), new RegExp(`^完成事项 ${todaySum(hDone)}`))
    assert.match(await head.innerText(), new RegExp(`完成 ${todaySum(hDone)} 件`))
    assert.ok(calls.some((c) => c.includes("metric=done") && c.includes("range=today") && c.includes("bucket=hour")))
    assert.ok(calls.some((c) => c.includes("metric=cost") && c.includes("range=today") && c.includes("bucket=hour")))
    const g = page.locator('[data-daily] [data-chart="group"]')
    assert.equal(await g.locator("[data-bar]").count(), 24)
    assert.equal(await g.locator("[data-current]").getAttribute("data-current"), hours[CUR])
    // the hours to come draw nothing, not even the line; the axis reads local hours
    for (let h = CUR + 1; h < 24; h++) assert.equal(await g.locator(`[data-bar="${hours[h]}"] path`).count(), 0, `hour ${h} is empty`)
    const ticks = await g.locator("svg > text:not([data-axis-right])").evaluateAll((xs) => xs.map((x) => x.textContent))
    assert.equal(ticks[0], "00:00"); assert.ok(ticks.every((t) => /^\d\d:00$/.test(t)), ticks.join(","))
    // the table: every hour, 未到 after now, and the done column sums to the day's total
    await head.locator("[data-chart-mode]").click()
    const rows = await page.locator('[data-daily] [data-table-row]').evaluateAll((xs) => xs.map((x) => [...x.querySelectorAll("td")].map((td) => td.textContent)))
    assert.equal(rows.length, 24)
    assert.deepEqual(rows[0], ["00:00–01:00", "0", "$0"])
    assert.deepEqual(rows[CUR], ["14:00–15:00未结束", "1", "$0.20"])
    assert.deepEqual(rows[23], ["23:00–24:00", "未到", "未到"])
    assert.equal(sum(rows.map((r) => Number(r[1]) || 0)), todaySum(hDone))
    await head.locator("[data-chart-mode]").click()
    await page.waitForFunction(() => { const c = document.querySelector('[data-daily] [data-chart="group"]'), v = c?.querySelector("svg"); return !!v && Math.round(v.getBoundingClientRect().width) === c.clientWidth && Number(v.getAttribute("width")) === c.clientWidth })
    if (width === 1440) {
      const at = async (h) => { await g.locator("svg").scrollIntoViewIfNeeded(); const box = await g.locator("svg").boundingBox(); return [box.x + 48 + (box.width - 104) * ((h + 0.5) / 24), box.y + 100] }
      await page.mouse.move(...(await at(20)))
      await page.locator("[data-chart-tip] [data-tip-future]").waitFor()
      assert.match(await page.locator("[data-chart-tip]").innerText(), /11月5日 20:00–21:00[\s\S]*还没到这个小时/)
      await page.mouse.move(...(await at(CUR)))
      await page.locator("[data-chart-tip] [data-tip-current]").waitFor()
      assert.match(await page.locator("[data-chart-tip]").innerText(), /14:00–15:00[\s\S]*未结束，截至[\s\S]*完成事项\s*1[\s\S]*估算费用\s*\$0\.20/)
      // an hour drills to that hour's items, the same count as its bar
      await page.mouse.move(...(await at(9)))
      assert.match(await page.locator("[data-chart-tip]").innerText(), /11月5日 09:00–10:00[\s\S]*完成事项\s*1[\s\S]*点击看这个小时的明细/)
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
    }
    const past = await page.evaluate(() => [...document.querySelectorAll("[data-chart] svg, [data-chart-head], [data-chart-head] *")].filter((e) => e.getBoundingClientRect().right > innerWidth + 1).map((e) => `${Math.round(e.getBoundingClientRect().right)} ${e.outerHTML.slice(0, 160)}`))
    assert.deepEqual(past, [], "by hour: nothing past the right edge")
    await page.locator('[data-stats-toolbar] [data-seg="7d"]').click()
    await page.locator('[data-daily] [data-chart="group"] [data-bar]').nth(days.length - 1).waitFor()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
    await page.close()
  }

  /* an item's drill-down: where the cost went — its own turns and the sub-items it started, in the order they began */
  for (const theme of ["light", "dark"]) for (const width of [1440, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", (e) => errors.push(e.message))
    const t0 = Date.now() - 5 * H, U = (input, output, est, x = {}) => ({ input, output, cache_read: 0, cache_write: 0, total: input + output, hit_rate: null, est, reported: 0, unpriced_tokens: 0, rough: false, ...x })
    const turn = (id, at, min, usage, x = {}) => ({ id, session_id: "ses_" + id, started: t0 + at * 60000, ended: t0 + (at + min) * 60000, stop_reason: "end_turn", attempt: 0, transient: false, tools: 3, tool_errors: 0, usage, ...x })
    const facts = { created: t0, first_turn: t0, first_deliver: null, last_deliver: null, accepted: null, done_at: null, dropped_at: null, lead_ms: null, cycle_ms: null, wait_ms: 0, agent_ms: 60000, turns: 2, delivers: 0, reworks: 0, first_pass: null, fails: 0, stalls: 0, releases: 0, release_fails: 0, decisions: 0, yolo_decided: 0 }
    const detail = {
      "ST-1": { item_id: "ST-1", no: 1, title: "重整统计页图表", status: "run", facts, usage: U(3000, 300, 0.5, { unpriced_tokens: 500000, reported: 2.5 }), age_ms: 5 * H, turns: [turn("a", 0, 2, U(1000, 100, 0.2)), turn("b", 30, 3, U(2000, 200, 0.3, { unpriced_tokens: 500000, reported: 2.5 }), { stop_reason: "error", tool_errors: 1 })], marks: [], decisions: [], by_model: [] },
      "ST-2": { item_id: "ST-2", no: 2, title: "整理发布脚本", status: "done", facts: { ...facts, done_at: t0 + 3 * H }, usage: U(10000, 1000, 1.25), age_ms: 3 * H, turns: [turn("c", 10, 5, U(10000, 1000, 1.25))], marks: [], decisions: [], by_model: [] },
    }
    const tree = { nodes: [{ item_id: "ST-1", parent_id: null, depth: 0, tokens: 3300, est: 0.5, reported: 0, agent_ms: 300000, first_turn: t0, done_at: null, dropped_at: null }, { item_id: "ST-2", parent_id: "ST-1", depth: 1, tokens: 11000, est: 1.25, reported: 0, agent_ms: 300000, first_turn: t0 + 10 * 60000, done_at: t0 + 3 * H, dropped_at: null }], truncated: false }
    await page.route((u) => u.pathname.startsWith("/api/"), (route) => {
      const u = new URL(route.request().url()), id = decodeURIComponent(u.pathname.split("/").pop())
      const data = u.pathname.startsWith("/api/stats/tree/") ? tree : u.pathname.startsWith("/api/stats/items/") ? detail[id] : null
      return data ? route.fulfill({ contentType: "application/json", body: JSON.stringify(data) }) : route.fulfill({ status: 404, contentType: "application/json", body: "{}" })
    })
    await page.goto(base + `/test/fixtures/stats.html?view=item&id=ST-1${theme === "dark" ? "&theme=dark" : ""}`)
    const cw = page.locator("[data-cost-waterfall]")
    await cw.locator('[data-cost-item="ST-2"]').waitFor()
    // the total is the item's and its sub-item's, the steps run in the order they began, the sub-item starts folded
    assert.match(await cw.locator(".ui-cwfc__stats").innerText(), /\$1\.75/)
    assert.match(await cw.innerText(), /1 件子事项/)
    assert.deepEqual(await cw.locator("[data-turn], [data-cost-item]").evaluateAll((els) => els.filter((e) => !e.closest("[data-hidden]")).map((e) => e.getAttribute("data-turn") ?? e.getAttribute("data-cost-item"))), ["a", "ST-2", "b"])
    assert.equal(await cw.locator('[data-cost-item="ST-2"]').getAttribute("aria-expanded"), "false")
    assert.equal(await cw.locator('[data-turn="b"]').getAttribute("data-status"), "failed")
    // unpriced tokens are marked where the cost shows, not only in the row's muted text: on the turn, and under the total
    assert.equal(await cw.locator('[data-turn="b"] [data-col="cost"] [data-unpriced-mark]').count(), 1)
    assert.equal(await cw.locator('[data-turn="a"] [data-unpriced-mark]').count(), 0)
    assert.match(await cw.locator("[data-unpriced-total]").innerText(), /另有 500K tokens 未定价/)
    // the most expensive step is a sub-item's turn: named with its sub-item, so it can't be taken for the item's own turn
    assert.match(await cw.locator(".ui-cwfc__top-name").innerText(), /^ST-2 · 回合 \d\d:\d\d$/)
    // unfolding the sub-item shows its own turn; a turn's details lead to its conversation
    await cw.locator('[data-cost-item="ST-2"] .ui-cwfc__chev').click()
    assert.equal(await cw.locator('[data-cost-item="ST-2"]').getAttribute("aria-expanded"), "true")
    await cw.locator('[data-turn="c"]').click()
    assert.equal(await cw.locator("[data-turn-session]").getAttribute("href"), "#/sessions/ses_c")
    assert.match((await cw.locator(".ui-cwfc__detail").innerText()).replace(/\s+/g, " "), /工具 3 次[\s\S]*\$1\.25/)
    // the details split what the waterfall's "In" adds up (input with the cache)
    assert.match(await cw.locator(".ui-cwfc__detail").innerText(), /未缓存输入 10K/)
    // tokens: the same steps measured in tokens
    await cw.getByRole("tab", { name: "tokens" }).click()
    assert.equal(await cw.locator(".ui-cwfc").getAttribute("data-mode"), "tokens")
    const wide = await page.evaluate(() => [...document.querySelectorAll("[data-cost-waterfall] *")].filter((e) => e.getBoundingClientRect().right > innerWidth + 1).map((e) => e.className).slice(0, 3))
    assert.deepEqual(wide, [], "nothing past the right edge")
    if (shots) await cw.screenshot({ path: `${shots}/stats-cost-${width}-${theme}.png`, animations: "disabled" })
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
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "现在 line + YOLO run state; counts = drill lists", "key numbers vs the range before, on the first screen", "daily output: done bars + cost/turns line, day drill", "where: split by Bot/model, cost/time; top items open the cost drill", "Bot table: per-Bot summaries, sortable, row filters the page", "harness · model table: agent turns per harness × model", "event explorer folded, counts said once, window + kind + zoom", "paused alert with 恢复", "no horizontal overflow", "item drill: cost waterfall of turns and sub-items", "今天 by hour: 24 local hours, current marked, future empty, table sums = day totals, hour drill"] }))
} finally { await browser.close() }
