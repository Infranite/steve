// The calendar page: the full app with isolated, synthetic API fixtures (never a real Hub). Day, week, 3-day and agenda
// views, an event's details, the sources, the day's summary, the sidebar's next meeting, the page without a calendar.
// STEVE_TEST_WEB: the web dev server; STEVE_TEST_SHOTS: a directory for screenshots.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5215", shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

/* the reader's zone is Asia/Shanghai; Wednesday 18 November 2026, 10:12 there */
const TZ = "Asia/Shanghai"
const at = (d, h = 0, m = 0) => Date.UTC(2026, 10, d, h - 8, m)
const NOW = at(18, 10, 12)
const pad = (n) => String(n).padStart(2, "0")
const local = (ms) => { const d = new Date(ms + 8 * 3_600_000); return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}` }
const ws = { id: "ws_1", name: "Sample space", description: "", item_prefix: "ST", sort: 0, created: 0 }
const state = { version: "fixture", web_version: "fixture", owner: false, workspace: ws, workspaces: [ws], items: [], decisions: [], recent: [], bots: [], allBots: [], projects: [], tags: [], tagUses: {}, thread: [], settings: { theme: "light" }, sessions: [], devices: [], runtimes: [], notifications: [], notifySeen: 0 }

const cal = (id, name, extra = {}) => ({ id, connection_id: "conn_ics", name, color: null, kind: "primary", role: "owner", share: "private", party_id: "p_me", party_name: "Morgan", party_set: true, mine: true, detail: "full", enabled: true, tz: TZ, synced_at: NOW - 2 * 60_000, sync_state: "ok", sync_error: null, gone: false, ...extra })
function sources() {
  return [
    { connection_id: "conn_ics", adapter: "ics", status: "active", features: [], ics: { subscribed: true, file: null }, calendars: [cal("cal_work", "工作"), cal("cal_home", "个人")] },
    { connection_id: "conn_team", adapter: "ics", status: "active", features: [], ics: { subscribed: true, file: null }, calendars: [
      cal("cal_team", "团队", { connection_id: "conn_team", sync_state: "error", sync_error: "连接超时", synced_at: NOW - 3 * 3_600_000 }),
      cal("cal_chen", "陈默 · 工作", { connection_id: "conn_team", party_id: "p_chen", party_name: "陈默", mine: false, detail: "busy", role: "free_busy" }),
      cal("cal_shared", "共享 · 会议室", { connection_id: "conn_team", party_id: null, party_name: null, party_set: false, mine: false, detail: "busy" }),
    ] },
  ]
}
const NAMES = { cal_work: "工作", cal_home: "个人", cal_team: "团队" }
let seq = 0
const ev = (d, h, m, h2, m2, title, extra = {}) => {
  const s = at(d, h, m), e = at(d, h2, m2), c = extra.cal ?? "cal_work"
  return { id: extra.id ?? "ev_" + ++seq, meeting_id: null, calendar: { id: c, name: NAMES[c] }, title, all_day: false, start_ms: s, end_ms: e, start: local(s), end: local(e), status: "confirmed", busy_only: false, version: 1, ...extra, cal: undefined }
}
function events() {
  seq = 0
  const xs = []
  for (const d of [16, 17, 18, 19, 20]) xs.push(ev(d, 9, 30, 9, 45, "站会 · 平台组", { attendees: 6, location: "3 楼小会议室" }))
  xs.push(ev(16, 11, 0, 12, 0, "招聘面试 · 后端"), ev(16, 15, 0, 16, 0, "合作周会", { cal: "cal_team" }))
  xs.push(ev(17, 13, 0, 14, 0, "1:1 陈默"))
  xs.push(ev(18, 10, 30, 11, 30, "远帆资本 · Q4 沟通", { id: "ev_yf", meeting_url: "https://meet.example/q4", location: "线上", attendees: 4, also_in: ["团队"], organizer: { name: null, email: null, party_id: "p_me", me: true } }))
  xs.push(ev(18, 12, 0, 13, 0, "午饭 · 林舟", { cal: "cal_home" }))
  xs.push(ev(18, 14, 0, 15, 0, "Hub 协调器评审", { id: "ev_hub", conflicts_with: ["ev_nb"] }))
  xs.push(ev(18, 14, 30, 15, 0, "北岸科技 · 接口对齐", { id: "ev_nb", my_rsvp: "needs_action", status: "tentative", conflicts_with: ["ev_hub"] }))
  xs.push(ev(18, 16, 0, 17, 0, "专注 · 写 Q4 复盘初稿"))
  xs.push(ev(18, 19, 0, 20, 0, "家长会", { cal: "cal_home" }))
  xs.push(ev(19, 10, 0, 11, 0, "设计评审 · 待办页"), ev(19, 13, 30, 14, 0, "供应商介绍", { my_rsvp: "declined" }), ev(19, 15, 0, 16, 0, "季度预算", { cal: "cal_team" }))
  xs.push(ev(20, 11, 0, 12, 0, "周会 · 平台组"), { ...ev(20, 0, 0, 0, 0, "发布日"), all_day: true, start: "2026-11-20", end: "2026-11-20" })
  xs.push(ev(21, 10, 0, 12, 0, "羽毛球", { cal: "cal_home" }))
  return xs
}
const detail = (e) => ({ ...e, attendees: e.id === "ev_yf" ? [
  { party_id: "p_me", email: "morgan@example.com", name: "Morgan", ext_ref: null, rsvp: "accepted", organizer: true, optional: false, is_self: true },
  { party_id: "p_zhou", email: "zhou@partner.example", name: "周可", ext_ref: null, rsvp: "accepted", organizer: false, optional: false, is_self: false },
  { party_id: null, email: "xu@partner.example", name: "许岚", ext_ref: null, rsvp: "tentative", organizer: false, optional: false, is_self: false },
  { party_id: "p_lin", email: null, name: "林舟", ext_ref: null, rsvp: "needs_action", organizer: false, optional: true, is_self: false },
] : [], excerpt: e.id === "ev_yf" ? "议程：\n1. Q3 收入口径\n2. 下一轮时间表" : undefined, as_of: local(NOW - 2 * 60_000) })

let n = 0
const todo = (text, extra = {}) => ({ id: "td_" + ++n, text, state: "open", item_id: null, matter: null, due: null, due_quote: null, due_kind: null, due_date: null, source: "you", writer: "reader", created: 0, updated: 0, closed: null, kind: "todo", parent: null, target_date: null, remind_at: null, snooze_until: null, block_event: null, pri: 0, star: false, recur: null, progress: null, sources: [], people: [], ...extra })
const todos = () => { n = 0; return [
  todo("回复周可「Q3 数据口径确认」", { due_date: "2026-11-16" }),
  todo("报销单提交", { due: at(18, 18) }),
  todo("给陈默写 1:1 反馈", { due_date: "2026-11-19" }),
  todo("Hub 协调器上线", { id: "goal1", kind: "goal", target_date: "2026-11-20", progress: { done: 3, open: 7, dropped: 0, total: 10, pct: 30 } }),
] }

const errors = []
/* mode: "full" (the fixture above), "none" (no source yet), "unwired" (a Hub without calendars) */
async function open({ theme = "light", width = 1440, hash = "/#/calendar", lang = "zh-CN", mode = "full", zone = TZ } = {}) {
  const m = { calls: [], events: events(), todos: todos() }
  const page = await browser.newPage({ viewport: { width, height: 1000 }, colorScheme: theme, reducedMotion: "reduce", locale: lang, timezoneId: zone })
  page.setDefaultTimeout(12000)
  await page.clock.setFixedTime(NOW)
  page.on("pageerror", (e) => errors.push(e.message))
  await page.addInitScript(({ theme, lang }) => { localStorage.setItem("steve-lang", lang); localStorage.setItem("steve-onboarded", "1"); localStorage.setItem("steve-theme", theme); document.addEventListener("DOMContentLoaded", () => document.documentElement.classList.toggle("dark", theme === "dark")) }, { theme, lang })
  await page.route((url) => url.pathname === "/auth/hello" || url.pathname.startsWith("/api/"), async (route) => {
    const req = route.request(), url = new URL(req.url()), path = url.pathname, method = req.method()
    const body = req.postData() ? req.postDataJSON() : null
    m.calls.push({ method, path: path + url.search, body })
    const json = (value, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(value) }).catch(() => {})
    if (path === "/auth/hello") return json({ name: "steve", api: 1, version: "fixture", login: { token: true, oidc: false } })
    if (path === "/api/state") return json({ ...state, settings: { theme } })
    if (path === "/api/me") return json({ user: { id: "reader", name: "Morgan", email: null, role: "member" }, version: "fixture", web_version: "fixture" })
    if (path === "/api/plugins/themes") return json({ themes: [] })
    if (path === "/api/onboard") return json({ state: "done" })
    if (path === "/api/digest/todos") return json({ todos: m.todos })
    if (path.startsWith("/api/calendar/")) {
      if (mode === "unwired") return json({}, 404)
      if (path === "/api/calendar/sources") return json(mode === "none" ? [] : sources())
      const sync = /^\/api\/calendar\/sources\/([^/]+)\/sync$/.exec(path)
      if (sync) return json(sources().find((s) => s.connection_id === sync[1]))
      if (path === "/api/calendar/events") {
        const from = url.searchParams.get("from") ?? "2026-11-18", to = url.searchParams.get("to") ?? from
        const inRange = m.events.filter((e) => (e.all_day ? e.end >= from && e.start <= to : e.start.slice(0, 10) <= to && e.end.slice(0, 10) >= from))
        return json({ range: { from: from + " 00:00", to: to + " 24:00", tz: TZ, from_ms: 0, to_ms: 0 }, events: mode === "none" ? [] : inRange, calendars: [] })
      }
      const one = /^\/api\/calendar\/events\/([^/]+)$/.exec(path)
      if (one) { const e = m.events.find((x) => x.id === decodeURIComponent(one[1])); return e ? json(detail(e)) : json({ error: "没有这个日程" }, 404) }
      if (path === "/api/calendar/find-time") return json({ range: {}, duration_min: body.duration_min, slots: [[11, 30], [13, 0], [17, 0]].map(([h, mm]) => ({ start: "", end: "", start_ms: at(18, h, mm), end_ms: at(18, h, mm + 30), why: [] })) })
    }
    return json({ error: "fixture route absent" }, 404)
  })
  await page.goto(base + hash)
  return { page, m }
}
const fits = async (page) => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
const shot = async (page, name) => { await fits(page); if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true }) }
const ids = (page, sel, attr) => page.locator(sel).evaluateAll((xs, a) => xs.map((x) => x.getAttribute(a)), attr)

try {
  /* every view in both themes, wide and on a phone */
  for (const theme of ["light", "dark"]) {
    {
      const { page, m } = await open({ theme })
      await page.locator('[data-cal-view="day"] [data-cal-event="ev_yf"]').waitFor()
      assert.equal(await page.locator("[data-nav-next]").innerText(), "10:30", "the sidebar names the next meeting's time")
      assert.equal(await page.locator("[data-cal-label]").innerText(), "11月18日 周三")
      assert.equal(await page.locator('[data-cal-event="ev_nb"]').getAttribute("data-cal-invite"), "true")
      assert.equal(await page.locator("[data-cal-conflict]").count(), 2)
      await page.locator("[data-cal-now]").waitFor()
      assert.equal(await page.locator("[data-cal-sum-head]").innerText(), "7 场会，1 处冲突，1 个邀请等你回复，1 个待办到期，1 个逾期。")
      assert.match(await page.locator('[data-cal-sum="meetings"]').innerText(), /14:00 与 14:30 冲突/)
      assert.match(await page.locator('[data-cal-sum="free"]').innerText(), /11:30–12:00，13:00–13:30，17:00–17:30/)
      assert.match(await page.locator('[data-cal-sum="goals"]').innerText(), /Hub 协调器上线 30% · 11月20日/)
      assert.match(await page.locator('[data-cal-next="ev_yf"]').innerText(), /18 分钟后 · 10:30/)
      assert.equal(await page.locator('[data-cal-next="ev_yf"] [data-cal-join]').getAttribute("href"), "https://meet.example/q4")
      assert.deepEqual(await ids(page, "[data-cal-due] [data-todo]", "data-todo"), ["td_1", "td_2"], "overdue and due today")
      assert.match(await page.locator("[data-cal-fresh]").innerText(), /数据截至 3 小时前/)
      assert.equal(await page.locator("[data-cal-fresh-failed]").innerText(), "1 本日历同步失败")
      assert.equal(await page.locator("[data-cal-zone]").count(), 0, "the browser is in the reader's zone")
      assert.ok(m.calls.some((c) => c.method === "POST" && c.path === "/api/calendar/sources/conn_ics/sync"), "opening the page asks the sources for changes")
      await shot(page, `calendar-day-1440-${theme}`)

      /* an event beside the day */
      await page.locator('[data-cal-event="ev_yf"]').click()
      await page.waitForURL(/#\/calendar\/e\/ev_yf\?view=day$/)
      await page.locator('[data-cal-panel] [data-cal-detail="ev_yf"]').waitFor()
      assert.deepEqual(await ids(page, "[data-cal-person]", "data-cal-person"), ["Morgan", "周可", "许岚", "林舟"])
      assert.deepEqual(await ids(page, "[data-cal-rsvp]", "data-cal-rsvp"), ["accepted", "accepted", "tentative", "needs_action"])
      assert.equal(await page.locator("[data-cal-also]").innerText(), "也出现在：团队")
      assert.equal(await page.locator("[data-cal-detail-join]").getAttribute("href"), "https://meet.example/q4")
      assert.equal(await page.locator('[data-cal-person="周可"]').getAttribute("href"), "#/contacts/p_zhou")
      await shot(page, `calendar-event-1440-${theme}`)
      await page.locator("[data-cal-panel-close]").click()
      await page.waitForURL(/#\/calendar\?view=day$/)

      /* the week: the month, the sources by person, seven days with deadlines and a goal */
      await page.locator('[data-seg="week"]').click()
      await page.locator('[data-cal-view="week"] [data-cal-grid="7"]').waitFor()
      assert.equal(await page.locator("[data-cal-label]").innerText(), "11月16日 – 11月22日")
      assert.deepEqual(await ids(page, "[data-cal-day-head]", "data-cal-day-head"), ["2026-11-16", "2026-11-17", "2026-11-18", "2026-11-19", "2026-11-20", "2026-11-21", "2026-11-22"])
      assert.equal(await page.locator('[data-cal-allday-cell="2026-11-20"] [data-cal-mark="goal"]').count(), 1)
      assert.equal(await page.locator('[data-cal-allday-cell="2026-11-16"] [data-cal-mark="todo"]').getAttribute("class").then((c) => c.includes("text-destructive")), true, "overdue in red")
      assert.equal(await page.locator('[data-cal-source-state="error"]').innerText(), "同步失败")
      assert.equal(await page.locator('[data-cal-source="cal_chen"] [data-cal-source-toggle]').count(), 0, "a contact's calendar gives free/busy only and is not drawn")
      assert.equal(await page.locator('[data-cal-month-day="2026-11-18"]').getAttribute("aria-current"), "date")
      assert.equal(await page.locator('[data-cal-sources-nobody] [data-cal-source="cal_shared"]').count(), 1, "a calendar nobody was named for is listed apart")
      await shot(page, `calendar-week-1440-${theme}`)
      /* hiding a calendar hides its events, and stays hidden */
      const homes = await page.locator('[data-cal-event]').filter({ hasText: "羽毛球" }).count()
      assert.equal(homes, 1)
      await page.locator('[data-cal-source-toggle="cal_home"]').click()
      await page.waitForFunction(() => ![...document.querySelectorAll("[data-cal-event]")].some((x) => x.textContent.includes("羽毛球")))
      assert.deepEqual(await page.evaluate(() => JSON.parse(localStorage.getItem("steve-cal-hidden"))), ["cal_home"])
      await page.locator('[data-cal-source-toggle="cal_home"]').click()
      /* a day head opens that day */
      await page.locator('[data-cal-day-head="2026-11-19"]').click()
      await page.waitForURL(/#\/calendar\?view=day&d=2026-11-19$/)
      await page.locator('[data-cal-summary="2026-11-19"]').waitFor()
      assert.equal(await page.locator("[data-cal-next]").count(), 0, "another day has no next meeting")

      /* the agenda */
      await page.goto(base + "/#/calendar?view=agenda")
      await page.locator("[data-cal-agenda]").waitFor()
      assert.equal(await page.locator("[data-cal-label]").innerText(), "11月18日 – 12月1日")
      assert.deepEqual((await ids(page, "[data-cal-agenda-day]", "data-cal-agenda-day")).slice(0, 4), ["2026-11-18", "2026-11-19", "2026-11-20", "2026-11-21"])
      await shot(page, `calendar-agenda-1440-${theme}`)
      await page.close()
    }
    {
      /* a phone: the agenda first, under a week strip; an event is its own page; 3 日 for conflicts */
      const { page } = await open({ theme, width: 390 })
      await page.locator('[data-cal-view="agenda"] [data-cal-strip]').waitFor()
      assert.deepEqual(await ids(page, "[data-mobile-tabs] a", "data-tab"), ["chat", "inbox", "calendar", "todos", "items"])
      assert.equal(await page.locator('[data-tab="calendar"]').getAttribute("aria-current"), "page")
      assert.deepEqual(await ids(page, "[data-cal-toolbar] [data-seg]", "data-seg"), ["agenda", "day", "three"])
      await page.locator('[data-cal-next="ev_yf"]').waitFor()
      await shot(page, `calendar-agenda-390-${theme}`)
      await page.locator('[data-cal-agenda-event="ev_hub"]').click()
      await page.locator('[data-cal-detail="ev_hub"]').waitFor()
      assert.equal(await page.locator("[data-cal-panel]").count(), 0)
      await shot(page, `calendar-event-390-${theme}`)
      await page.locator("[data-back]").first().click()
      await page.locator("[data-cal-agenda]").waitFor()
      await page.locator('[data-seg="three"]').click()
      await page.locator('[data-cal-grid="3"]').waitFor()
      await shot(page, `calendar-three-390-${theme}`)
      await page.locator('[data-seg="day"]').click()
      await page.locator('[data-cal-grid="1"]').waitFor()
      await shot(page, `calendar-day-390-${theme}`)
      /* a week link becomes 3 日 on a phone */
      await page.goto(base + "/#/calendar?view=week")
      await page.locator('[data-cal-grid="3"]').waitFor()
      await page.close()
    }
  }

  /* the browser in another zone: the page still shows the reader's times, and says which zone */
  {
    const { page } = await open({ zone: "America/New_York" })
    await page.locator('[data-cal-event="ev_yf"]').waitFor()
    assert.equal(await page.locator("[data-cal-zone]").innerText(), "按 Asia/Shanghai 显示")
    await page.close()
  }
  /* a browser zone with the same clock (Asia/Taipei) needs no note */
  {
    const { page } = await open({ zone: "Asia/Taipei" })
    await page.locator('[data-cal-event="ev_yf"]').waitFor()
    assert.equal(await page.locator("[data-cal-zone]").count(), 0)
    assert.equal(await page.locator("[data-nav-next]").innerText(), "10:30")
    assert.equal(await page.locator("[data-cal-label]").innerText(), "11月18日 周三")
    await page.close()
  }

  /* no calendar yet, and a Hub without calendars */
  for (const width of [1440, 390]) {
    const { page } = await open({ mode: "none", width })
    await page.locator("[data-calendar-prompt]").waitFor()
    assert.equal(await page.locator("[data-calendar-deadlines] [data-todo]").count(), 3)
    assert.equal(await page.locator("[data-nav-next]").count(), 0)
    await shot(page, `calendar-none-${width}-light`)
    await page.locator("[data-calendar-connect]").click()
    await page.waitForURL(/#\/integrations\/ics$/)
    await page.close()
  }
  {
    const { page } = await open({ mode: "unwired" })
    await page.getByText("这个 Hub 还不支持日历").waitFor()
    assert.equal(await page.locator("[data-calendar-connect]").count(), 0)
    await page.close()
  }

  /* English */
  {
    const { page } = await open({ lang: "en", hash: "/#/calendar?view=week" })
    await page.locator('[data-cal-grid="7"]').waitFor()
    assert.equal(await page.locator("[data-cal-label]").innerText(), "Nov 16 – Nov 22")
    assert.equal(await page.locator('[data-cal-source-state="error"]').innerText(), "Sync failed")
    await page.goto(base + "/#/calendar?view=day")
    await page.locator("[data-cal-sum-head]").waitFor()
    assert.equal(await page.locator("[data-cal-sum-head]").innerText(), "7 meetings, 1 conflict, 1 invitation to answer, 1 todo due, 1 overdue.")
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("calendar.browser: ok")
} finally {
  await browser.close()
}
