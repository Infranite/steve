// Real StatsView + /api/stats/realtime contract fixtures, not a live token collector.
// Run against this checkout's Vite server (STEVE_TEST_WEB); optional screenshots go to STEVE_TEST_SHOTS.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { setTimeout as delay } from "node:timers/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197", shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const NOW = Date.parse("2026-11-15T12:00:00Z"), errors = [], passed = []
const eventually = async (fn, label = "condition") => {
  for (let i = 0; i < 250; i++) { if (await fn()) return; await delay(20) }
  assert.fail(`Timed out: ${label}`)
}
const value = (p) => p.locator("[data-realtime] .ui-kpic__amount").innerText()
const hasValue = (p, v) => eventually(async () => await value(p) === v, `TPS = ${v}`)
const hasReason = (p, reason) => eventually(async () => await p.locator("[data-realtime]").getAttribute("data-realtime-reason") === reason, reason)
const choose = async (p, filter, name) => {
  await p.locator(`[data-select="stats-${filter}"]`).click()
  await p.getByRole("option", { name, exact: true }).click()
}
function reading(at, c) {
  const idle = c.mode === "idle" || (c.stopAt !== undefined && at - c.stopAt >= 10000)
  const silent = c.silentAt !== undefined && at - c.silentAt > 3000
  const state = idle ? "idle" : c.mode === "partial" || silent ? "partial" : c.mode === "estimated" ? "estimated" : c.mode === "unavailable" ? "unavailable" : "measured"
  const unknown = state === "unavailable" || state === "partial"
  const tps = unknown ? null : idle ? 0 : c.tps ?? 12.5
  const end = Math.floor(at / 1000) * 1000
  const series = Array.from({ length: 60 }, (_, i) => ({ at: end - (59 - i) * 1000, tps, state }))
  if (c.gaps) {
    series[10] = { ...series[10], tps: null, state: "unavailable" }
    // An absent second must leave the same gap as explicit null.
    series.splice(12, 1)
    for (let i = 20; i < 25; i++) series[i] = { ...series[i], tps: 8, state: "estimated" }
    series[35] = { ...series[35], tps: null, state: "partial" }
  }
  return { at, window_ms: 10000, step_ms: 1000, tps, output_sessions: unknown ? null : idle ? 0 : c.sessions ?? 2, state,
    coverage: (state === "partial" ? c.partialCoverage : c.coverage) ?? { measured: state === "estimated" ? 0 : 1, estimated: state === "estimated" ? 2 : 1, unavailable: unknown ? 1 : 0 }, series,
    estimated_tps: unknown ? c.observedTps ?? 99 : null,
    observed_output_sessions: unknown ? 1 : 0,
    // The contract explicitly permits additive backend fields.
    source: "synthetic-browser-fixture" }
}
async function fixture({ lang = "zh-CN", width = 1200, theme = "light", past = false, control = {} } = {}) {
  const page = await browser.newPage({ locale: lang, viewport: { width, height: 1000 }, reducedMotion: "reduce", colorScheme: theme })
  page.on("pageerror", (e) => errors.push(e.message))
  await page.clock.install({ time: new Date(NOW) })
  await page.clock.pauseAt(new Date(NOW))
  const c = { mode: "measured", ...control }, requests = [], history = [], held = [], failed = []
  page.on("requestfailed", (r) => { if (r.url().includes("/stats/realtime")) failed.push(r.url()) })
  await page.route((u) => u.pathname.startsWith("/api/"), async (route) => {
    const u = new URL(route.request().url())
    if (u.pathname !== "/api/stats/realtime") {
      history.push(u)
      return route.fulfill({ status: 404, contentType: "application/json", body: "{}" })
    }
    requests.push(u)
    if (c.mode === "hold") return new Promise((resolve) => held.push({ route, resolve }))
    if (c.mode === "network") return route.abort("failed")
    if (c.mode === "404" || c.mode === "503") return route.fulfill({ status: Number(c.mode), contentType: "application/json", body: "{}" })
    if (c.mode === "html") return route.fulfill({ contentType: "text/html", body: "<!doctype html><title>Fallback</title>" })
    const at = c.frozenAt ?? await page.evaluate(() => Date.now())
    const data = c.mode === "invalid" ? { tps: 0 } : c.mode === "invalidTimestamp" ? { ...reading(at, c), at: 1e308 } : c.mode === "nulls" ? {
      ...reading(at, c), tps: null, output_sessions: null, state: "partial",
      series: reading(at, c).series.map((p) => ({ ...p, tps: null, state: "unavailable" })),
    } : reading(at, c)
    return route.fulfill({ contentType: "application/json", body: JSON.stringify(data) })
  })
  await page.goto(`${base}/test/fixtures/stats-realtime.html?theme=${theme}${past ? "&past=1" : ""}`)
  await page.locator("[data-realtime]").waitFor()
  const tick = async (ms = 1000) => { await page.clock.runFor(ms); await delay(30) }
  const release = async (v = 999) => {
    for (const h of held.splice(0)) {
      await h.route.fulfill({ contentType: "application/json", body: JSON.stringify(reading(NOW, { tps: v })) }).catch(() => {})
      h.resolve()
    }
  }
  return { page, c, requests, history, failed, held, tick, release }
}
try {
  // A historical date range and even a missing summary API cannot stop realtime data.
  {
    const { page, c, requests, history, tick } = await fixture({ past: true, control: { sessions: 1, gaps: true } })
    await hasValue(page, "12.5")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), "1")
    assert.match(await page.locator("[data-now]").innerText(), /历史/, "the historical range keeps its upstream status label")
    assert.ok(history.some((u) => u.pathname.endsWith("/summary") && u.searchParams.get("from") === "2020-01-01"))
    assert.match(await page.locator("[data-realtime-coverage]").innerText(), /实测 1 · 估算 1 · 不可用 0/)
    const path = await page.locator(".ui-kpic__lines path").first().getAttribute("d")
    assert.ok((path.match(/M/g) ?? []).length >= 3, "null and omitted seconds break the line")
    await page.locator("[data-realtime-table] summary").click()
    const rows = await page.locator("[data-realtime-table] tbody tr").allTextContents()
    assert.equal(rows.length, 60)
    assert.match(rows[10], /缺失不可用/); assert.match(rows[12], /缺失不可用/)
    await page.locator("[data-realtime-table] summary").click()
    await page.locator(".ui-kpic__plot").focus()
    await page.keyboard.press("ArrowRight")
    assert.match(await page.locator('.ui-kpic__tooltip[data-open="true"]').innerText(), /实测[\s\S]*12\.5/)
    await page.keyboard.press("Escape")
    await page.getByRole("button", { name: "TPS 与输出会话数的定义" }).focus()
    await page.getByRole("tooltip").waitFor()
    assert.match(await page.getByRole("tooltip").innerText(), /最近 10 秒[\s\S]*token[\s\S]*唯一会话/)
    await page.locator("[data-realtime]").evaluate((el) => { el.dataset.sameMount = "yes" })
    for (const range of ["today", "30d", "7d", "custom"]) {
      const n = requests.length
      await page.locator(`[data-stats-toolbar] [data-seg="${range}"]`).click()
      assert.equal(await page.locator("[data-realtime]").getAttribute("data-same-mount"), "yes")
      assert.equal(requests.length, n, "historical ranges do not restart polling")
    }
    const n = requests.length
    c.sessions = 2; c.tps = 25
    await tick(); await hasValue(page, "25")
    assert.equal(requests.length, n + 1, "polls once per second")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), "2")
    assert.ok(requests.every((u) => u.search === ""), "no history, model or source filters sent")
    passed.push("single/concurrent readings; gaps; keyboard and tooltip; past/custom/today/30d/7d independence; 1s polling")
    await page.close()
  }
  // Filtering clears the value synchronously and aborts the old request, including late answers.
  {
    const { page, c, requests, failed, held, release } = await fixture()
    await hasValue(page, "12.5")
    assert.equal(await page.locator("[data-now] [data-live]").innerText(), "实时")
    c.mode = "hold"
    await choose(page, "project", "Project Alpha")
    await eventually(() => held.length > 0)
    await hasValue(page, "—")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), "—")
    c.mode = "estimated"; c.tps = 22
    await choose(page, "project", "Project Beta")
    await hasValue(page, "≈ 22")
    await release()
    assert.equal(await value(page), "≈ 22", "late Alpha values cannot leak into Beta")
    assert.ok(failed.some((url) => url.includes("project=p1")), "the Alpha request was aborted")
    c.mode = "partial"; c.tps = 30
    await choose(page, "bot", "Reviewer")
    await hasReason(page, "partial"); await hasValue(page, "—")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), "—")
    assert.match(await page.locator("[data-realtime]").innerText(), /输出会话总数待确认/)
    assert.match(await page.locator("[data-realtime-message]").innerText(), /总数暂无法确认/)
    assert.ok(requests.some((u) => u.searchParams.get("project") === "p2" && u.searchParams.get("bot") === "rev"))
    assert.ok(requests.every((u) => [...u.searchParams.keys()].every((k) => ["project", "bot"].includes(k))))
    passed.push("project/Bot parameters; abort and late-response isolation; estimated and partial labels")
    await page.close()
  }
  // Real backend partial responses have unknown totals, even when they expose observed subtotals.
  // A single silent session is incomplete time coverage, not evidence that some other session exists.
  for (const lang of ["zh-CN", "en"]) for (const available of [0, 1]) {
    const coverage = { measured: 0, estimated: available, unavailable: 1 }
    const { page, c, tick } = await fixture({ lang, control: { mode: "estimated", sessions: available + 1,
      silentAt: NOW, coverage: { measured: 0, estimated: available + 1, unavailable: 0 }, partialCoverage: coverage, observedTps: 99 } })
    await hasValue(page, "≈ 12.5")
    for (let i = 0; i < 3; i++) await tick()
    assert.equal(await value(page), "≈ 12.5", "collection stays fresh through three seconds")
    await tick(); await hasReason(page, "partial"); await hasValue(page, "—")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), "—")
    assert.match(await page.locator("[data-realtime] dl").innerText(), lang === "en"
      ? /Output session total in the last 10s is unconfirmed/ : /最近 10 秒的输出会话总数待确认/)
    assert.match(await page.locator("[data-realtime-coverage]").innerText(), lang === "en"
      ? new RegExp(`measured 0 · estimated ${available} · unavailable 1`) : new RegExp(`实测 0 · 估算 ${available} · 不可用 1`))
    const message = await page.locator("[data-realtime-message]").innerText()
    assert.match(message, lang === "en" ? /totals cannot be confirmed/ : /总数暂无法确认/)
    assert.match(message, lang === "en" ? /does not mean the model has stopped/ : /不代表模型已停止/)
    assert.doesNotMatch(message, /Only some sessions|Values cover|部分会话|数值仅含/)
    assert.doesNotMatch(await page.locator("[data-realtime] .ui-kpic__headline").innerText(), /99/)
    assert.equal(await page.locator(".ui-kpic__plot[data-empty]").count(), 1, "observed subtotals do not become a total TPS trend")
    const missing = lang === "en" ? "Missing" : "缺失"
    assert.deepEqual(await page.locator("[data-realtime-table] tbody td:first-of-type").allTextContents(), Array(60).fill(missing))
    await page.getByRole("button", { name: lang === "en" ? "How TPS and output sessions are counted" : "TPS 与输出会话数的定义" }).focus()
    await page.getByRole("tooltip").waitFor()
    assert.match(await page.getByRole("tooltip").innerText(), lang === "en" ? /3 seconds[\s\S]*5 seconds/ : /3 秒[\s\S]*5 秒/)
    delete c.silentAt
    await tick(); await hasReason(page, "estimated"); await hasValue(page, "≈ 12.5")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), String(available + 1))
    await page.close()
  }
  passed.push("single-session silence and multiple-session missing coverage: null totals stay unknown despite observed subtotals; Chinese/English; 3s collection vs 5s response freshness")
  // The frontend follows the backend's 10s window, including the tail after output stops.
  {
    const { page, c, tick } = await fixture({ control: { stopAt: NOW } })
    await hasValue(page, "12.5")
    for (let i = 0; i < 9; i++) await tick()
    assert.equal(await value(page), "12.5")
    await tick(); await hasReason(page, "idle"); await hasValue(page, "0")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), "0")
    assert.equal(await page.locator(".ui-kpic__plot[data-empty]").count(), 0, "idle zero is a real reading")
    assert.match(await page.locator(".ui-kpic__lines path").nth(2).getAttribute("d"), /L/, "zero history draws its baseline")
    c.mode = "measured"; delete c.stopAt; c.tps = 0.002
    await tick(); await hasValue(page, "0.002")
    passed.push("stopped output expires at 10s per contract; true idle zero is drawn; small TPS does not round to zero")
    await page.close()
  }
  for (const mode of ["404", "503", "html", "network", "invalid", "invalidTimestamp", "unavailable", "nulls"]) {
    const { page, c, tick } = await fixture()
    await hasValue(page, "12.5")
    c.mode = mode
    await tick(); await hasReason(page, mode.startsWith("invalid") ? "invalid" : mode === "unavailable" ? "unavailable" : mode === "nulls" ? "partial" : "request")
    await hasValue(page, "—")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), "—")
    assert.equal(await page.locator(".ui-kpic__plot[data-empty]").count(), 1)
    assert.ok((await page.locator(".ui-kpic__lines path").evaluateAll((xs) => xs.map((x) => x.getAttribute("d")))).every((d) => !d))
    c.mode = "measured"; c.tps = 15
    await tick(); await hasValue(page, "15")
    await page.close()
  }
  passed.push("404/503/HTML/network/invalid/unavailable/null fields clear prior metrics and never draw fake zeros; automatic recovery")
  {
    const { page, c, tick } = await fixture({ control: { mode: "idle", frozenAt: NOW } })
    await hasValue(page, "0")
    for (let i = 0; i < 6; i++) await tick()
    await hasReason(page, "stale"); await hasValue(page, "—")
    assert.equal(await page.locator("[data-realtime-sessions]").innerText(), "—")
    assert.match(await page.locator("[data-realtime-message]").innerText(), /超过 5 秒/)
    delete c.frozenAt
    await tick(); await hasReason(page, "idle"); await hasValue(page, "0")
    // A server clock too far ahead cannot be presented as live either.
    c.frozenAt = NOW + 60000
    await tick(); await hasReason(page, "stale"); await hasValue(page, "—")
    passed.push("cached timestamp ages out despite successful polls; stale idle zero hidden; recovery; future clock rejected")
    await page.close()
  }
  {
    const { page, c, requests, held, failed, tick, release } = await fixture()
    await hasValue(page, "12.5")
    c.mode = "hold"
    await tick(); await eventually(() => held.length > 0)
    const n = requests.length
    await tick(2000)
    assert.equal(requests.length, n, "slow requests do not overlap")
    await tick(2000); await hasReason(page, "request"); await hasValue(page, "—")
    assert.ok(failed.length > 0, "slow request timed out and was aborted")
    c.mode = "measured"; c.tps = 18
    await release(); await tick(); await hasValue(page, "18")
    c.mode = "hold"; await tick(); await eventually(() => held.length > 0)
    const abortedBefore = failed.length
    await page.locator("[data-fixture-unmount]").click()
    const end = requests.length
    await tick(3000)
    assert.equal(requests.length, end, "no polling after unmount")
    assert.equal(await page.locator("[data-realtime]").count(), 0)
    // Chromium can defer requestfailed while Playwright holds the route. Release interception
    // before checking cancellation; an un-aborted response would complete instead of failing.
    await release()
    await eventually(() => failed.length > abortedBefore, "unmount aborts its in-flight request")
    passed.push("hung requests time out without overlap, recover, abort on unmount and stop polling")
    await page.close()
  }
  // Real layout in both languages and themes, including 320px. Screenshot banner declares synthetic data.
  for (const lang of ["zh-CN", "en"]) for (const theme of ["light", "dark"]) for (const width of [1440, 375, 320]) {
    const { page } = await fixture({ lang, width, theme, control: { gaps: true, mode: "partial" } })
    await hasValue(page, "—")
    await page.locator("[data-realtime-table] summary").click()
    const outside = await page.evaluate(() => [...document.querySelectorAll("[data-realtime], [data-realtime] *")].filter((el) => {
      const box = el.getBoundingClientRect(); return getComputedStyle(el).display !== "none" && box.width > 0 && (box.right > innerWidth + 1 || box.left < -1)
    }).map((el) => el.className))
    assert.deepEqual(outside, [], `${lang}/${theme}/${width}: no realtime horizontal overflow`)
    assert.equal(await page.locator("[data-realtime-table] th[scope=col]").count(), 3)
    assert.match(await page.locator(".ui-kpic__plot").getAttribute("aria-label"), lang === "en" ? /arrow keys/ : /方向键/)
    await page.locator("[data-realtime-table] summary").click()
    assert.equal(await page.locator("[data-realtime-table]").getAttribute("open"), null)
    assert.equal(await page.locator("[data-realtime-table]").evaluate((el) => getComputedStyle(el, "::details-content").transitionDuration), "0s", "respects reduced motion for disclosure")
    if (shots) await page.screenshot({ path: `${shots}/realtime-fixture-${lang}-${theme}-${width}.png`, animations: "disabled" })
    await page.close()
  }
  passed.push("Chinese/English × light/dark × 1440/375/320px; per-second accessible table; no overflow")
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, source: "synthetic API contract fixtures, no real token collector verified", scenarios: passed }, null, 2))
} finally { await browser.close() }
