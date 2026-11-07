// Vite fixture only. Every API request is intercepted; no Hub, account or model is used.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
const at = Date.now()
const coverage = { retention_days: 7, earliest_cursor: 1, latest_cursor: 3, instance_pending: 0, pending_scope: "instance", recorder_available: true, delivery: "best_effort", unobserved_gaps: "unknown", loss_count_stable: true, refresh_required: false }
const event = (id, cursor, extra = {}) => ({ event_id: id, cursor, at: at - cursor * 1000, kind: "dependency.resume", reason_code: "open_dependency", severity: "info", session_id: "session-one", attempt_id: "attempt-one", details: { dependency: "ST-100" }, ...extra })
const first = event("event-3", 3), earlier = event("event-2", 2)
const complete = (events, rest = {}) => ({ events, next_cursor: null, has_more: false, coverage, gaps: [], truncated: false, ...rest })
const settle = (page) => page.evaluate(() => Promise.allSettled(document.getAnimations().filter((a) => a.effect?.getTiming().iterations !== Infinity).map((a) => a.finished)))

async function setup(page, { owner = true, lang = "zh-CN" } = {}) {
  const requests = [], mutations = []
  let capture = { active: false, started_at: null, expires_at: null, stop_reason: null, bytes: 0, dropped: 0 }
  let failNext = false, pendingSlow = null
  const explain = () => ({ observed_at: Date.now(), item: { id: "ST-123", status: "block", phase: "active", cycle: 2, revision: 4 }, capture, sessions: [{ id: "session-one" }, { id: "session-two" }], coverage, can_capture: owner,
    state: { lifecycle: { blockers: [{ kind: "dependency", label: "ST-100 尚未完成" }] }, dependencies: { open_dependencies: ["ST-100"], resume_mode: "automatic", resume_source: "workspace" }, lane: { summary: "没有等待中的发布" } } })
  await page.route((url) => url.pathname.startsWith("/api/"), async (route) => {
    const request = route.request(), url = new URL(request.url())
    if (url.pathname === "/api/items/ST-123/debug") {
      if (request.method() === "POST") {
        const body = request.postDataJSON(); mutations.push(body)
        capture = { ...capture, active: body.enabled, started_at: Date.now(), expires_at: body.enabled ? Date.now() + body.minutes * 60_000 : null, stop_reason: body.enabled ? null : "disabled" }
      }
      return route.fulfill({ json: explain() })
    }
    if (url.pathname === "/api/items/ST-123/debug/events") {
      requests.push(Object.fromEntries(url.searchParams))
      if (url.searchParams.get("kinds") === "slow") {
        pendingSlow = () => route.fulfill({ json: complete([event("stale-event", 1)]) }).catch(() => {})
        return
      }
      if (url.searchParams.get("kinds") === "empty") return route.fulfill({ json: complete([]) })
      if (url.searchParams.get("kinds") === "updating") return route.fulfill({ json: complete([first], { coverage: { ...coverage, loss_count_stable: false, refresh_required: true } }) })
      if (url.searchParams.get("kinds") === "pending") return route.fulfill({ json: complete([first], { coverage: { ...coverage, instance_pending: 3 } }) })
      if (url.searchParams.get("kinds") === "unavailable") return route.fulfill({ json: complete([first], { coverage: { ...coverage, recorder_available: false } }) })
      if (url.searchParams.get("kinds") === "gaps") return route.fulfill({ json: complete([first], { truncated: true, gaps: [{ reason: "expired", before: 2 }] }) })
      if (url.searchParams.get("cursor")) {
        if (failNext) { failNext = false; return route.fulfill({ status: 503, json: { error: "暂时不可用" } }) }
        return route.fulfill({ json: complete([first, earlier]) })
      }
      return route.fulfill({ json: complete([first], { has_more: true, next_cursor: "snapshot:3:before:3" }) })
    }
    return route.fulfill({ status: 404, json: { error: "fixture endpoint unavailable" } })
  })
  page.on("pageerror", (e) => errors.push(e.message))
  await page.goto(base + "/test/fixtures/debug.html?lang=" + lang)
  return { requests, mutations, failMore: () => { failNext = true }, releaseSlow: async () => { if (pendingSlow) await pendingSlow() } }
}

try {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1066, height: 900 } })
  const data = await setup(page)
  await page.getByRole("button", { name: "更多", exact: true }).click()
  await page.locator("[data-item-debug]").click()
  await page.locator('[data-debug-event="event-3"]').waitFor()
  assert.match(await page.locator("[data-debug-delivery]").innerText(), /无法检测的缺口/)
  assert.equal(await page.locator("[data-debug-pending], [data-debug-recorder-unavailable], [data-debug-coverage-refresh]").count(), 0)
  assert.match(await page.locator("[data-debug-blockers]").innerText(), /ST-100 尚未完成/)
  assert.equal(await page.locator("[data-debug-minutes]").inputValue(), "30")
  await page.locator("[data-debug-toggle]").click()
  await page.getByRole("button", { name: "停止记录", exact: true }).waitFor()
  assert.deepEqual(data.mutations, [{ enabled: true, minutes: 30 }])
  await page.locator("[data-debug-toggle]").click()
  await page.getByRole("button", { name: "开启详细记录", exact: true }).waitFor()
  assert.deepEqual(data.mutations[1], { enabled: false })

  data.failMore()
  await page.locator("[data-debug-more]").click()
  await page.locator("[data-debug-events-error]").waitFor()
  assert.equal(await page.locator("[data-debug-event]").count(), 1, "a failed page keeps already visible evidence")
  await page.locator("[data-debug-events-error]").getByRole("button").click()
  await page.locator('[data-debug-event="event-2"]').waitFor()
  assert.equal(await page.locator("[data-debug-event]").count(), 2, "replayed boundary event is not duplicated")
  assert.equal(data.requests.at(-1).cursor, "snapshot:3:before:3", "cursor stays opaque")

  await Promise.all([
    page.waitForRequest((r) => new URL(r.url()).searchParams.get("kinds") === "slow"),
    page.locator("[data-debug-kind]").fill("slow"),
  ])
  await page.locator("[data-debug-kind]").fill("empty")
  await page.locator("[data-debug-empty]").waitFor()
  await data.releaseSlow()
  assert.equal(await page.locator('[data-debug-event="stale-event"]').count(), 0, "old filter response cannot populate the current view")
  assert.match(await page.locator("[data-debug-empty]").innerText(), /不代表没有发生问题/)
  await page.locator("[data-debug-kind]").fill("updating")
  await page.locator("[data-debug-coverage-refresh]").waitFor()
  assert.match(await page.locator("[data-debug-coverage-refresh]").innerText(), /部分计数暂不稳定，请刷新/)
  assert.equal(await page.locator("[data-debug-event]").count(), 1, "updating counts do not hide committed evidence")
  await page.locator("[data-debug-kind]").fill("pending")
  await page.locator("[data-debug-pending]").waitFor()
  assert.equal(await page.locator("[data-debug-coverage-refresh]").count(), 0)
  assert.match(await page.locator("[data-debug-pending]").innerText(), /实例还有 3 条.*其他事项/)
  await page.locator("[data-debug-kind]").fill("unavailable")
  await page.locator("[data-debug-recorder-unavailable]").waitFor()
  assert.match(await page.locator("[data-debug-recorder-unavailable]").innerText(), /已有证据仍可查看/)
  assert.equal(await page.locator("[data-debug-pending]").count(), 0)
  assert.equal(await page.locator("[data-debug-event]").count(), 1, "recorder failure does not hide existing evidence")
  await page.locator("[data-debug-kind]").fill("gaps")
  await page.locator("[data-debug-gaps]").waitFor()
  assert.equal(await page.locator("[data-debug-recorder-unavailable]").count(), 0)
  await page.locator("[data-debug-detail] summary").click()
  assert.match(await page.locator("[data-debug-detail]").innerText(), /event-3/)
  await page.evaluate(() => { Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (text) => { window.copiedEvidence = text } } }) })
  await page.locator("[data-debug-copy]").click()
  const reference = JSON.parse(await page.evaluate(() => window.copiedEvidence))
  assert.equal(reference.event_id, "event-3")
  assert.equal(reference.item_id, "ST-123")
  await settle(page)
  if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/debug-desktop.png` })
  await page.keyboard.press("Escape")
  await page.locator("[data-fixture-switch]").click()
  await page.locator("[data-session-menu]").click()
  await page.locator("[data-session-debug]").click()
  await page.locator('[data-debug-event="event-3"]').waitFor()
  assert.equal(await page.locator("[data-debug-session]").inputValue(), "session-two")
  assert.equal(data.requests.at(-1).session_id, "session-two")
  await page.close()

  const mobile = await browser.newPage({ locale: "en-US", viewport: { width: 390, height: 844 }, colorScheme: "dark" })
  await setup(mobile, { owner: false, lang: "en" })
  await mobile.getByRole("button", { name: "More", exact: true }).click()
  await mobile.locator("[data-item-debug]").click()
  await mobile.locator('[data-debug-event="event-3"]').waitFor()
  assert.match(await mobile.locator("[data-debug-delivery]").innerText(), /undetected gaps/)
  await mobile.locator("[data-debug-kind]").fill("updating")
  await mobile.locator("[data-debug-coverage-refresh]").waitFor()
  assert.match(await mobile.locator("[data-debug-coverage-refresh]").innerText(), /counts are temporarily unstable.*Please refresh/)
  await mobile.locator("[data-debug-kind]").fill("pending")
  await mobile.locator("[data-debug-pending]").waitFor()
  assert.equal(await mobile.locator("[data-debug-coverage-refresh]").count(), 0)
  assert.match(await mobile.locator("[data-debug-pending]").innerText(), /3 records.*across the instance/)
  await mobile.locator("[data-debug-kind]").fill("unavailable")
  await mobile.locator("[data-debug-recorder-unavailable]").waitFor()
  assert.match(await mobile.locator("[data-debug-recorder-unavailable]").innerText(), /existing evidence is still available/)
  await settle(mobile)
  assert.equal(await mobile.locator("[data-debug-toggle]").count(), 0, "read-only viewers do not get a misleading capture control")
  assert.equal(await mobile.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
  assert.equal(await mobile.locator("[data-debug-dialog]").evaluate((el) => el.scrollWidth > el.clientWidth), false)
  if (process.env.STEVE_TEST_SHOTS) {
    await mobile.locator("[data-debug-recorder-unavailable]").scrollIntoViewIfNeeded()
    await mobile.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/debug-mobile-dark.png` })
  }
  await mobile.close()
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["item/session entry", "capture readback", "failed-page retry", "opaque cursor and dedupe", "stale-filter response", "missing evidence", "gaps", "updating/pending/unavailable/unknown gaps in zh/en", "evidence reference", "owner visibility", "desktop/mobile"] }))
} finally { await browser.close() }
