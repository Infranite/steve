// Browser smoke for sidebar IA order, pinned footer Stats entry, disambiguated naming (zh-CN / en),
// mobile tabs & More view, and durable navigation usage (7d / 30d / all) on the Stats page.
// Runs against synthetic API fixtures only.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"

const TZ = "Asia/Shanghai"
const at = (d, h = 0, m = 0) => Date.UTC(2026, 10, d, h - 8, m)
const NOW = at(18, 10, 12)
const pad = (n) => String(n).padStart(2, "0")
const local = (ms) => { const d = new Date(ms + 8 * 3_600_000); return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}` }

const ws = { id: "ws_1", name: "Sample space", description: "", item_prefix: "ST", sort: 0, created: 0, role: "owner" }
const shell = { version: "fixture", web_version: "fixture", owner: true, workspace: ws, workspaces: [ws], bots: [], allBots: [], projects: [], tags: [], tagUses: {}, settings: { theme: "light" }, devices: [], runtimes: [], notifications: [], notifySeen: 0 }
const decisions = [
  { id: "dec_1", ws: "ws_1", kind: "approve", subject_type: "item", subject_id: "ST-1", item_id: "ST-1", session_id: null, question: { kind: "raw", text: "Approve deploy?" }, draft: "", options: [], status: "open", result: null, requested_by: "steve", created: NOW - 60_000, resolved: null, undo_until: null, space_id: null, tier: "decide", folded: null, revision: 1 },
  { id: "dec_2", ws: "ws_1", kind: "ask", subject_type: "item", subject_id: "ST-2", item_id: "ST-2", session_id: null, question: { kind: "raw", text: "Which target?" }, draft: "", options: [], status: "open", result: null, requested_by: "steve", created: NOW - 120_000, resolved: null, undo_until: null, space_id: null, tier: "decide", folded: null, revision: 1 },
]
const todos = [
  { id: "td_1", text: "Reply to Morgan", state: "open", item_id: null, matter: null, due: null, due_quote: null, due_kind: null, due_date: "2026-11-16", source: "you", writer: "owner", created: 0, updated: 0, closed: null, kind: "todo", parent: null, target_date: null, remind_at: null, snooze_until: null, block_event: null, pri: 0, star: false, recur: null, progress: null, sources: [], people: [] },
  { id: "td_2", text: "Submit report", state: "open", item_id: null, matter: null, due: at(18, 18), due_quote: null, due_kind: null, due_date: null, source: "you", writer: "owner", created: 0, updated: 0, closed: null, kind: "todo", parent: null, target_date: null, remind_at: null, snooze_until: null, block_event: null, pri: 1, star: false, recur: null, progress: null, sources: [], people: [] },
]
const calSources = [{ connection_id: "c1", adapter: "ics", status: "active", features: [], ics: { subscribed: true, file: null }, calendars: [{ id: "cal_1", connection_id: "c1", name: "Work", color: null, kind: "primary", role: "owner", share: "private", party_id: "p1", party_name: "Owner", party_set: true, mine: true, detail: "full", enabled: true, tz: TZ, synced_at: NOW - 60_000, sync_state: "ok", sync_error: null, gone: false }] }]
const calEvents = [{ id: "ev_1", meeting_id: null, calendar: { id: "cal_1", name: "Work" }, title: "Sync", all_day: false, start_ms: at(18, 10, 30), end_ms: at(18, 11, 0), start: local(at(18, 10, 30)), end: local(at(18, 11, 0)), status: "confirmed", busy_only: false, version: 1 }]

const emptySummary = {
  range: { from: "2026-11-12", to: "2026-11-18", tz: TZ },
  usage: { coverage: { state: "complete", expected: 0, reported: 0, complete: 0, pending: 0, unavailable: 0, total: 0, input: 0, output: 0, cache_read: 0, cache_write: 0, cost_reported: 0 }, input: 0, output: 0, cache_read: 0, cache_write: 0, reasoning: 0, total: 0, hit_rate: null, est: 0, reported: 0, unpriced_tokens: 0, saved_by_cache: 0, calls: 0, failed_calls: 0, turns: 0, turn_ms: 0, turn_errors: 0, retries: 0, interrupted: 0, tools: 0, tool_errors: 0 },
  spend: { est: 0, reported: 0, unpriced_tokens: 0, unpriced: {}, saved_by_cache: 0, by_source: {}, gateway_by_purpose: {}, priced_by: {}, catalog: { synced: null, tried: null, error: null, models: 0 } },
  throughput: { created: 0, created_by_source: {}, done: 0, dropped: 0, drop_rate: null, wip: 0, delivered: 0, delivered_auto: 0, delivered_manual: 0, net: 0 },
  time: { lead: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, cycle: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, first_deliver: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, wait: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, agent: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, other: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, releases: 0, released: 0, release_success_rate: null, not_released: 0, accept_to_release: { p50: null, p85: null, avg: null, sum: 0, n: 0 } },
  quality: { first_pass: null, first_pass_n: 0, reworks: 0, reworks_per_done: null, unit_retries: 0, fails: {}, stalled_then_done: null, rollbacks: 0, release_unknown: 0, turn_error_rate: null, reopened: 0 },
  human: { decisions: 0, by_kind: {}, response: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, response_by_kind: {}, expired: 0, approval: { sessions: 0, turns: 0, permissions: 0, permissions_by_resolver: {}, questions: 0 }, messages: 0, per_done: null },
  agents: { turns: 0, turn_ms: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, tools: 0, tool_error_rate: null, tool_kinds: {}, retries: 0, retry_success: null, gateway_fallbacks: 0, gateway_failed: 0, permissions: 0, interrupted: 0, cancelled: 0, tokens_per_turn: null, tools_per_turn: null },
  per_item: { tokens: { p50: null, p85: null, avg: null, sum: 0, n: 0 }, est_p50: null, est_avg: null, est_top: [] },
  workflow: null, context_over_80: 0, since: { tokens: null, gateway: null }, prev: null,
}

const errors = []
async function open({ lang = "zh-CN", width = 1440, height = 900, hash = "/#/inbox" } = {}) {
  const navCounts = new Map()
  const navRanges = []
  const page = await browser.newPage({ viewport: { width, height }, colorScheme: "light", reducedMotion: "reduce", locale: lang, timezoneId: TZ })
  page.setDefaultTimeout(12000)
  await page.clock.setFixedTime(NOW)
  page.on("pageerror", (e) => errors.push(e.message))
  await page.addInitScript((lang) => {
    localStorage.setItem("steve-lang", lang)
    localStorage.setItem("steve-onboarded", "1")
    localStorage.setItem("steve-theme", "light")
  }, lang)
  await page.route((u) => u.pathname === "/auth/hello" || u.pathname.startsWith("/api/"), async (route) => {
    const req = route.request(), url = new URL(req.url()), path = url.pathname
    const json = (v, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(v) }).catch(() => {})
    if (path === "/auth/hello") return json({ name: "steve", api: 2, version: "fixture", login: { token: true, oidc: false } })
    if (path === "/api/shell") return json(shell)
    if (path === "/api/snapshot") return json({ epoch: "ep1", rev: 1, cursor: "1", views: { decision: decisions, item: [], session: [], message: { views: [], next: null, rev: 1 } } })
    if (path === "/api/stream" || path === "/api/events") return route.fulfill({ status: 200, contentType: "text/event-stream", body: ": ok\n\n" }).catch(() => {})
    if (path === "/api/me") return json({ user: { id: "owner", name: "Owner", email: null, role: "owner" }, version: "fixture", web_version: "fixture" })
    if (path === "/api/people/me/avatar") return route.fulfill({ status: 404, body: "" }).catch(() => {})
    if (path === "/api/plugins/themes") return json({ themes: [] })
    if (path === "/api/onboard") return json({ state: "done" })
    if (path === "/api/hub/info") return json({ version: "fixture", web_version: "fixture", update: null })
    if (path === "/api/digest/todos") return json({ todos })
    if (path === "/api/calendar/sources") return json(calSources)
    if (path === "/api/calendar/events") return json({ range: { from: "2026-11-18 00:00", to: "2026-11-18 24:00", tz: TZ, from_ms: 0, to_ms: 0 }, events: calEvents, calendars: [] })
    if (path === "/api/approval-policy") return json({ policy: { mode: "ask_owner", revision: 1 }, editable: true })
    if (path === "/api/github/repos") return json([])
    if (path === "/api/system/web-perf" && req.method() === "POST") {
      const body = req.postDataJSON()
      for (const [k, n] of Object.entries(body?.nav_opens ?? {})) navCounts.set(k, (navCounts.get(k) ?? 0) + Number(n))
      return json({ ok: true })
    }
    if (path === "/api/stats/summary") return json(emptySummary)
    if (path === "/api/stats/nav") {
      const range = url.searchParams.get("range") || "7d"
      navRanges.push(range)
      const merged = new Map(navCounts)
      if (range === "30d" || range === "all") merged.set("chat", (merged.get("chat") ?? 0) + 10)
      if (range === "all") merged.set("chat", (merged.get("chat") ?? 0) + 20)
      const items = [...merged.entries()].map(([item, opens]) => ({ item, opens })).sort((a, b) => b.opens - a.opens || a.item.localeCompare(b.item))
      const from = range === "all" ? "2026-10-01" : range === "30d" ? "2026-10-20" : "2026-11-12"
      return json({ range, from, to: "2026-11-18", total: items.reduce((s, x) => s + x.opens, 0), items })
    }
    return json({})
  })
  await page.goto(base + hash)
  return { page, navCounts, navRanges }
}

const EXPECTED = {
  "zh-CN": {
    groups: [
      { id: "primary", label: null, items: [["chat", "对话"], ["inbox", "待我决定"], ["items", "事项"], ["projects", "项目"]] },
      { id: "schedule", label: "日程", items: [["calendar", "日历"], ["todos", "我的待办"]] },
      { id: "messages", label: "消息与人", items: [["feed", "信息流"], ["channels", "群聊与私聊"], ["mail", "邮件"], ["contacts", "联系人"]] },
      { id: "library", label: "资料", items: [["knowledge", "知识"], ["integrations", "连接"]] },
      { id: "automate", label: "Bot 与自动化", items: [["bots", "Bot"], ["sessions", "会话"], ["auto", "自动化"], ["capabilities", "能力"], ["signals", "信号"]] },
    ],
    footer: [["stats", "统计"]],
    titles: { inbox: "待我决定", todos: "我的待办", items: "事项" },
    ranges: { d30: "近 30 天", all: "全部" },
    tabs: [["chat", "对话"], ["inbox", "待我决定"], ["items", "事项"], ["calendar", "日历"], ["todos", "我的待办"]],
  },
  en: {
    groups: [
      { id: "primary", label: null, items: [["chat", "Chat"], ["inbox", "Decisions"], ["items", "Items"], ["projects", "Projects"]] },
      { id: "schedule", label: "Schedule", items: [["calendar", "Calendar"], ["todos", "My todos"]] },
      { id: "messages", label: "Messages & people", items: [["feed", "Feed"], ["channels", "Chats"], ["mail", "Mail"], ["contacts", "Contacts"]] },
      { id: "library", label: "Library", items: [["knowledge", "Knowledge"], ["integrations", "Connections"]] },
      { id: "automate", label: "Bots & automation", items: [["bots", "Bots"], ["sessions", "Conversations"], ["auto", "Automation"], ["capabilities", "Capabilities"], ["signals", "Signals"]] },
    ],
    footer: [["stats", "Stats"]],
    titles: { inbox: "Decisions", todos: "My todos", items: "Items" },
    ranges: { d30: "30 days", all: "All time" },
    tabs: [["chat", "Chat"], ["inbox", "Decisions"], ["items", "Items"], ["calendar", "Calendar"], ["todos", "My todos"]],
  },
}

try {
  for (const lang of ["zh-CN", "en"]) {
    const exp = EXPECTED[lang]
    const { page, navCounts, navRanges } = await open({ lang })
    await page.locator("[data-sidebar-nav]").waitFor()
    await page.locator('[data-nav-count="todos"]').waitFor()
    await page.locator("[data-nav-next]").waitFor()

    const groups = await page.locator("[data-sidebar-nav] [data-nav-group]").evaluateAll((gs) => gs.map((g) => ({
      id: g.getAttribute("data-nav-group"),
      label: g.querySelector('[data-sidebar="group-label"]')?.textContent?.trim() ?? null,
      items: [...g.querySelectorAll("[data-nav]")].map((a) => [a.getAttribute("data-nav"), a.textContent.trim()]),
    })))
    assert.deepEqual(groups, exp.groups, `${lang} desktop sidebar groups, order and labels`)

    const footer = await page.locator("[data-sidebar-footer-nav] [data-nav]").evaluateAll((as) => as.map((a) => [a.getAttribute("data-nav"), a.textContent.trim()]))
    assert.deepEqual(footer, exp.footer, `${lang} pinned footer nav`)

    // Stats entry is pinned in the footer and visible at a glance without scrolling
    const statsBox = await page.locator('[data-sidebar-footer-nav] [data-nav="stats"]').boundingBox()
    assert.ok(statsBox && statsBox.y >= 0 && statsBox.y + statsBox.height <= 900, `${lang} Stats footer entry visible at 1440×900`)

    // Badges stay attached to the right entries
    assert.equal((await page.locator('[data-nav="inbox"]').locator("xpath=..").locator('[data-sidebar="menu-badge"]').innerText()).trim(), "2")
    assert.equal((await page.locator('[data-nav-count="todos"]').innerText()).trim(), "2")
    assert.equal((await page.locator("[data-nav-next]").innerText()).trim(), "10:30")

    // Disambiguated page titles across 待我决定 / 事项 / 我的待办
    assert.equal((await page.locator("h1").innerText()).trim(), exp.titles.inbox)
    await page.locator('[data-nav="items"]').click()
    await page.waitForFunction((want) => document.querySelector("h1")?.textContent?.trim() === want, exp.titles.items)
    await page.locator('[data-nav="todos"]').click()
    await page.waitForFunction((want) => document.querySelector("h1")?.textContent?.trim() === want, exp.titles.todos)

    // Command palette uses consistent naming and order
    await page.keyboard.press("Meta+k")
    const palette = page.locator("[data-command-palette]")
    await palette.waitFor()
    assert.equal((await palette.locator('[data-cmd-page="inbox"]').innerText()).trim(), exp.titles.inbox)
    assert.equal((await palette.locator('[data-cmd-page="todos"]').innerText()).trim(), exp.titles.todos)
    assert.equal((await palette.locator('[data-cmd-page="items"]').innerText()).trim(), exp.titles.items)
    await page.keyboard.press("Escape")
    await palette.waitFor({ state: "detached" })

    // Opening Stats flushes navigation open counts and renders durable 7d / 30d / all views
    await page.locator('[data-sidebar-footer-nav] [data-nav="stats"]').click()
    const navStats = page.locator("[data-nav-usage]")
    await navStats.locator('[data-nav-usage-row="inbox"]').waitFor()
    assert.equal(navCounts.get("inbox"), 1)
    assert.equal(navCounts.get("items"), 1)
    assert.equal(navCounts.get("todos"), 1)
    assert.equal(navCounts.get("stats"), 1)
    assert.equal(await navStats.getAttribute("data-nav-usage"), "7d")
    assert.equal((await navStats.locator('[data-nav-usage-row="inbox"] [data-nav-usage-opens]').innerText()).trim(), "1")

    // Switch to 30d and all ranges
    const d30 = page.locator('[data-nav-usage-controls] [data-seg="30d"]')
    assert.equal((await d30.innerText()).trim(), exp.ranges.d30)
    await d30.click()
    await page.locator('[data-nav-usage="30d"] [data-nav-usage-row="chat"]').waitFor()
    assert.equal((await navStats.locator('[data-nav-usage-row="chat"] [data-nav-usage-opens]').innerText()).trim(), "10")

    const all = page.locator('[data-nav-usage-controls] [data-seg="all"]')
    assert.equal((await all.innerText()).trim(), exp.ranges.all)
    await all.click()
    await page.locator('[data-nav-usage="all"] [data-nav-usage-row="chat"]').waitFor()
    assert.equal((await navStats.locator('[data-nav-usage-row="chat"] [data-nav-usage-opens]').innerText()).trim(), "30")
    assert.deepEqual(navRanges.slice(-3), ["7d", "30d", "all"])
    await page.close()
  }

  // Short laptop viewport: pinned Stats entry remains visible at a glance without scrolling
  {
    const { page } = await open({ lang: "zh-CN", width: 1280, height: 700, hash: "/#/inbox" })
    await page.locator("[data-sidebar-footer-nav]").waitFor()
    const box = await page.locator('[data-sidebar-footer-nav] [data-nav="stats"]').boundingBox()
    assert.ok(box && box.y >= 0 && box.y + box.height <= 700, "Stats footer entry visible without scrolling at 1280×700")
    await page.close()
  }

  // Mobile: bottom tab order, naming, no overflow, and More view groups
  {
    const { page } = await open({ lang: "zh-CN", width: 390, height: 844, hash: "/#/inbox" })
    await page.locator("[data-mobile-tabs]").waitFor()
    const tabs = await page.locator("[data-mobile-tabs] a").evaluateAll((as) => as.map((a) => [a.getAttribute("data-tab"), a.querySelector("span")?.textContent?.trim()]))
    assert.deepEqual(tabs, EXPECTED["zh-CN"].tabs, "mobile tab bar order and labels")
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "mobile tabs fit without horizontal overflow")

    await page.locator("[data-mine]").click()
    await page.locator("[data-more-me]").waitFor()
    const moreGroups = await page.locator("[data-more-group]").evaluateAll((gs) => gs.map((g) => ({
      id: g.getAttribute("data-more-group"),
      items: [...g.querySelectorAll("[data-more-nav]")].map((r) => r.getAttribute("data-more-nav")),
    })))
    assert.deepEqual(moreGroups, [
      { id: "primary", items: ["projects", "publications"] },
      { id: "messages", items: ["feed", "channels", "mail", "contacts"] },
      { id: "library", items: ["knowledge", "integrations"] },
      { id: "automate", items: ["bots", "sessions", "auto", "capabilities", "signals"] },
      { id: "more", items: ["stats", "accounts", "vault", "settings"] },
    ])
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "mobile More page fits without horizontal overflow")
    await page.close()
  }

  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["zh-CN desktop IA & pinned Stats", "en desktop IA & pinned Stats", "1280×700 Stats visible", "badges preserved", "command palette naming", "nav telemetry -> Stats 7d/30d/all", "mobile tabs & More IA"] }))
} finally { await browser.close() }
