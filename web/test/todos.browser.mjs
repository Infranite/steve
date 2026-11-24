// The menu, the todo and goal pages and the calendar page without a calendar: the full app with isolated, synthetic API fixtures,
// never a real Hub. STEVE_TEST_WEB: the web dev server; STEVE_TEST_SHOTS: a directory for screenshots.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5215", shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })

/* Wednesday 18 November 2026, 10:00 in the browser's time zone */
const NOW = new Date(2026, 10, 18, 10, 0)
const at = (d, h = 0, m = 0) => new Date(2026, 10, d, h, m).getTime()
const ws = { id: "ws_1", name: "Sample space", description: "", item_prefix: "ST", sort: 0, created: 0 }
const itemState = (status) => status === "done" ? { kind: "closed", outcome: "finished", by: "owner" } : status === "prop" ? { kind: "ready" } : { kind: "active" }
const itemView = (no, title, status) => ({ id: "ST-" + no, ws: "ws_1", no, title, state: itemState(status), cycle: 1, revision: 1, delivery_revision: 0, delivery: [], waits: [], priority: "中", star: false, archived: false, design_gate: false, source: "", created: 0, updated: 0, goals: 1, tags: [], deps: [], plan_done: 0, plan_total: 0, op_blockers: [], detail_stamp: 0, active_operations: 0, has_branch: false })
const item = (no, title, status) => ({ id: "ST-" + no, workspace_id: "ws_1", no, title, status, stage: "", bot_id: null, project_id: null, cwd: null, branch: null, plan: [], now: "", accept_mode: null, priority: "中", archived: false, star: false, due: null, snooze: null, source: ["", ""], parent: null, created: 0, updated: 0, goal: title, goals: 1, tags: [], deps: [], sessionIds: [], open: [], queued: 0, live: false })
const itemViews = [itemView(402, "Coordinator skeleton", "done"), itemView(427, "Node heartbeat and reconnect", "run"), itemView(433, "Sort the November receipts", "run"), itemView(440, "Load test for 100 Nodes", "prop")]
const state = { version: "fixture", web_version: "fixture", owner: false, workspace: ws, workspaces: [ws], items: [item(402, "Coordinator skeleton", "done"), item(427, "Node heartbeat and reconnect", "run"), item(433, "Sort the November receipts", "run"), item(440, "Load test for 100 Nodes", "prop")], decisions: [], recent: [], bots: [], allBots: [], projects: [], tags: [], tagUses: {}, thread: [], settings: { theme: "light" }, sessions: [], devices: [], runtimes: [], notifications: [], notifySeen: 0 }

let seq = 0
const todo = (text, extra = {}) => ({ id: "td_" + ++seq, text, state: "open", item_id: null, matter: null, due: null, due_quote: null, due_kind: null, due_date: null, source: "you", writer: "reader", created: at(10) + seq, updated: at(10) + seq, closed: null, kind: "todo", parent: null, target_date: null, remind_at: null, snooze_until: null, block_event: null, pri: 0, star: false, recur: null, progress: null, sources: [], people: [], ...extra })
function fixture() {
  seq = 0
  const goal = todo("Ship the Hub coordinator by end of December", { id: "goal1", kind: "goal", target_date: "2026-12-31" })
  const review = todo("Q4 review", { id: "goal2", kind: "goal", target_date: "2026-12-05" })
  const g = { id: goal.id, text: goal.text }
  const todos = [goal, review,
    todo("Reply to Morgan about the Q3 data definitions", { due_date: "2026-11-14", source: "steve", sources: [{ conv: "mail_thread:m1", message: "mm1", name: "Mail", href: null, excerpt: "Can you confirm the Q3 numbers?" }] }),
    todo("Prepare a one-page funding update", { due: at(18, 10, 30), pri: 3 }),
    todo("Submit the expense report", { due: at(18, 18), star: true }),
    todo("Write the beta release notes", { due_date: "2026-11-20", parent: g }),
    todo("Book a badminton court for Saturday", { due_date: "2026-11-20", source: "agent" }),
    todo("Invite three early users", { due_date: "2026-11-25", parent: g }),
    todo("Read up on CalDAV incremental sync"),
    todo("Replace the home router", { snooze_until: at(23, 9) }),
    todo("Sort the November receipts", { state: "handed", item_id: "ST-433", closed: at(17, 15) }),
    todo("Settle the coordinator interface", { state: "done", parent: g, closed: at(6, 12) }),
  ]
  /* made: the items POST /items made, by client_id, as the Hub's send dedupe keeps them; failState: answer the next
   * hand's state change with an error */
  return { todos, links: { goal1: ["ST-402", "ST-427"] }, calls: [], made: new Map(), failState: false }
}
function progressOf(m, id) {
  const kids = m.todos.filter((x) => x.parent?.id === id), its = new Set(), states = []
  const itemState = (i) => { const s = state.items.find((x) => x.id === i)?.status; return s === "done" ? "done" : s === "drop" ? "dropped" : "open" }
  for (const k of kids) if (k.state === "handed") { if (!its.has(k.item_id)) { its.add(k.item_id); states.push(itemState(k.item_id)) } } else states.push(k.state === "done" ? "done" : k.state === "dropped" ? "dropped" : "open")
  for (const i of m.links[id] ?? []) if (!its.has(i)) { its.add(i); states.push(itemState(i)) }
  const n = (s) => states.filter((x) => x === s).length, counted = states.length - n("dropped")
  return { done: n("done"), open: n("open"), dropped: n("dropped"), total: states.length, pct: counted > 0 ? Math.floor(n("done") * 100 / counted) : null }
}
const view = (m, x) => ({ ...x, progress: x.kind === "goal" ? progressOf(m, x.id) : null })

const errors = []
async function open({ theme = "light", width = 1440, hash = "/#/todos", lang = "zh-CN" } = {}) {
  const m = fixture()
  const page = await browser.newPage({ viewport: { width, height: 1000 }, colorScheme: theme, reducedMotion: "reduce", locale: lang })
  page.setDefaultTimeout(12000)
  await page.clock.setFixedTime(NOW)
  page.on("pageerror", (e) => errors.push(e.message))
  await page.addInitScript(({ theme, lang }) => { localStorage.setItem("steve-lang", lang); localStorage.setItem("steve-onboarded", "1"); localStorage.setItem("steve-theme", theme); document.addEventListener("DOMContentLoaded", () => document.documentElement.classList.toggle("dark", theme === "dark")) }, { theme, lang })
  await page.route((url) => url.pathname === "/auth/hello" || url.pathname.startsWith("/api/"), async (route) => {
    const req = route.request(), url = new URL(req.url()), path = url.pathname, method = req.method()
    const body = req.postData() ? req.postDataJSON() : null
    if (method !== "GET") m.calls.push({ method, path, body })
    const json = (value, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(value) }).catch(() => {})
    if (path === "/auth/hello") return json({ name: "steve", api: 2, version: "fixture", login: { token: true, oidc: false } })
    if (path === "/api/shell") return json({ ...state, settings: { theme } })
    if (path === "/api/summary") return json({ ws: ws.id, thinking: false, hold: null })
    if (path === "/api/snapshot") return json({ epoch: "fixture", rev: 1, cursor: "fixture:1", views: { item: itemViews, session: [], decision: [], message: { views: [], next: null } } })
    if (path === "/api/views") return json({ rev: 1, views: [], missing: [] })
    if (path === "/api/notifications") return json([])
    if (path === "/api/conversations/channels") return json({ channels: [] })
    if (path === "/api/me") return json({ user: { id: "reader", name: "Morgan", email: null, role: "member" }, version: "fixture", web_version: "fixture" })
    if (path === "/api/plugins/themes") return json({ themes: [] })
    if (path === "/api/onboard") return json({ state: "done" })
    if (path === "/api/items" && method === "POST") {
      /* slow, so a second click lands while the first is on its way */
      await new Promise((r) => setTimeout(r, 300))
      if (!m.made.has(body.client_id)) m.made.set(body.client_id, item(500 + m.made.size, body.goal, "run"))
      return json(m.made.get(body.client_id))
    }
    /* no calendar connected: the calendar page is its connect prompt */
    if (path === "/api/calendar/sources") return json([])
    if (path === "/api/calendar/events") return json({ range: { from: "", to: "", tz: "UTC", from_ms: 0, to_ms: 0 }, events: [], calendars: [] })
    if (path === "/api/digest/todos" && method === "GET") return json({ todos: m.todos.map((x) => view(m, x)) })
    if (path === "/api/digest/todos" && method === "POST") {
      const x = todo(body.text, { kind: body.kind ?? "todo", due: body.due ?? null, due_date: body.due_date ?? null, pri: body.pri ?? 0, target_date: body.target_date ?? null, parent: body.parent ? { id: body.parent, text: m.todos.find((y) => y.id === body.parent)?.text } : null })
      m.todos.push(x); return json({ id: x.id, change: "ch_add_" + x.id })
    }
    const one = /^\/api\/digest\/todos\/([^/]+)(\/items)?$/.exec(path)
    if (one) {
      const x = m.todos.find((y) => y.id === decodeURIComponent(one[1]))
      if (!x) return json({ error: "没有这条待办" }, 404)
      if (one[2]) { m.links[x.id] = body.linked === false ? (m.links[x.id] ?? []).filter((i) => i !== body.item) : [...(m.links[x.id] ?? []), body.item]; return json({ changed: true, change: "ch_link" }) }
      if (method === "GET") return json(x.kind === "goal" ? { ...view(m, x), children: m.todos.filter((y) => y.parent?.id === x.id).map((y) => view(m, y)), items: m.links[x.id] ?? [] } : view(m, x))
      if (method === "PATCH") { Object.assign(x, body); return json({ ok: true, change: "ch_edit_" + x.id }) }
      if (method === "POST" && body.state === "handed" && m.failState) { m.failState = false; return json({ error: "暂时写不进去" }, 500) }
      if (method === "POST") { x.state = body.state; x.item_id = body.item ?? null; x.closed = body.state === "open" ? null : NOW.getTime(); return json({ ok: true, change: "ch_state_" + x.id, next: null }) }
    }
    if (path.startsWith("/api/digest/changes/") && path.endsWith("/undo")) {
      const id = path.split("/")[4].replace(/^ch_state_/, ""), x = m.todos.find((y) => y.id === id)
      if (x) { x.state = "open"; x.closed = null }
      return json({ ok: true })
    }
    return json({ error: "fixture route absent" }, 404)
  })
  await page.goto(base + hash)
  return { page, m }
}
const fits = async (page) => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
const shot = async (page, name) => { await fits(page); if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true }) }

try {
  /* the menu, the list, a goal, a todo and the calendar in both themes, wide and on a phone */
  for (const theme of ["light", "dark"]) for (const width of [1440, 390]) {
    const { page } = await open({ theme, width })
    await page.locator('[data-todo-group="overdue"]').waitFor()
    assert.deepEqual(await page.locator("[data-todo-group]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-todo-group"))), ["overdue", "today", "week", "none", "handed"])
    if (width === 1440) {
      assert.deepEqual(await page.locator('[data-sidebar-nav] [data-sidebar="group"]').first().locator("[data-nav]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-nav"))), ["chat", "feed", "inbox", "calendar", "todos", "items", "projects"])
      assert.equal(await page.locator('[data-nav-count="todos"]').innerText(), "3", "overdue and due today, in gray")
      assert.equal(await page.locator('[data-nav="todos"]').getAttribute("aria-current"), "page")
    } else {
      assert.deepEqual(await page.locator("[data-mobile-tabs] a").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-tab"))), ["chat", "inbox", "calendar", "todos", "items"])
      assert.equal(await page.locator('[data-tab="todos"]').getAttribute("aria-current"), "page")
      await page.locator("[data-mine]").waitFor()
    }
    await shot(page, `todos-${width}-${theme}`)
    await page.goto(base + "/#/todos?view=goals"); await page.locator('[data-goal-card="goal1"]').waitFor()
    await shot(page, `goals-${width}-${theme}`)
    await page.goto(base + "/#/todos/goal1"); await page.locator('[data-goal-page="goal1"]').waitFor()
    assert.equal(await page.locator("[data-goal-progress-card] [data-goal-progress]").getAttribute("data-goal-progress"), "40", "2 of 5: a done child and a done item")
    await shot(page, `goal-${width}-${theme}`)
    await page.goto(base + "/#/todos/td_4"); await page.locator('[data-todo-detail="td_4"]').waitFor()
    await shot(page, `todo-${width}-${theme}`)
    await page.goto(base + "/#/calendar"); await page.locator("[data-calendar-prompt]").waitFor()
    assert.equal(await page.locator("[data-calendar-deadlines] [data-todo]").count(), 5)
    await shot(page, `calendar-${width}-${theme}`)
    await page.close()
  }

  /* quick add: the rules' chips, a removed one, and what goes to the Hub */
  {
    const { page, m } = await open()
    const field = page.locator("[data-quick-add=todo] [data-quick-input]")
    await field.fill("周五前 交 Q4 复盘 18:00 #coordinator !2")
    await page.locator('[data-quick-chip="time"]').waitFor()
    assert.deepEqual(await page.locator("[data-quick-chip]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-quick-chip"))), ["time", "goal", "pri"])
    await page.locator('[data-quick-chip="pri"] button').click()
    await field.press("Enter")
    await page.getByText("交 Q4 复盘 !2").first().waitFor()
    assert.deepEqual(m.calls.at(-1), { method: "POST", path: "/api/digest/todos", body: { text: "交 Q4 复盘 !2", kind: "todo", parent: "goal1", due: at(20, 18) } })
    /* done, then undone from the toast */
    await page.locator('[data-todo="td_4"] [data-todo-check]').click()
    await page.getByText("完成：Prepare a one-page funding update").first().waitFor()
    assert.deepEqual(m.calls.at(-1), { method: "POST", path: "/api/digest/todos/td_4", body: { state: "done" } })
    await page.locator("li", { hasText: "完成：Prepare a one-page funding update" }).getByRole("button", { name: "撤销", exact: true }).click()
    await page.getByText("已撤销").first().waitFor()
    assert.equal(m.calls.at(-1).path, "/api/digest/changes/ch_state_td_4/undo")
    /* snooze until tomorrow morning */
    await page.locator('[data-todo="td_5"]').hover()
    await page.locator('[data-todo="td_5"] [data-todo-snooze]').click()
    await page.locator('[data-snooze="tomorrow"]').click()
    assert.deepEqual(m.calls.at(-1), { method: "PATCH", path: "/api/digest/todos/td_5", body: { snooze_until: at(19, 9) } })
    /* ⌘K: 新待办 opens the page with the field focused */
    await page.goto(base + "/#/inbox")
    await page.keyboard.press("Meta+k")
    await page.locator("[data-cmd-new-todo]").click()
    await page.locator("[data-quick-add=todo] [data-quick-input]").waitFor()
    assert.equal(await page.evaluate(() => document.activeElement?.hasAttribute("data-quick-input")), true)
    await page.close()
  }

  /* 交给 Bot: a double click makes one item, and a retry after the todo's half failed gets the same item back */
  {
    const { page, m } = await open({ hash: "/#/todos/td_7" })
    const hand = page.locator("[data-detail-hand]")
    m.failState = true
    await hand.dblclick()
    await page.getByText("暂时写不进去").first().waitFor()
    await page.waitForFunction(() => !document.querySelector("[data-detail-hand]")?.hasAttribute("disabled"))
    await hand.click()
    await page.getByText("已交给 Bot：事项 ST-500").first().waitFor()
    const posts = m.calls.filter((c) => c.path === "/api/items")
    assert.equal(posts.length, 2, "the double click sent once, the retry once more")
    assert.equal(posts[0].body.client_id, posts[1].body.client_id)
    assert.equal(posts[0].body.client_id, "todo-hand:td_7:" + m.todos.find((x) => x.id === "td_7").updated)
    assert.equal(m.made.size, 1, "one item")
    assert.deepEqual(m.calls.filter((c) => c.path === "/api/digest/todos/td_7").map((c) => c.body), [{ state: "handed", item: "ST-500" }, { state: "handed", item: "ST-500" }])
    await page.close()
  }

  /* a goal's page: link an item, it counts in the progress */
  {
    const { page, m } = await open({ hash: "/#/todos/goal1" })
    await page.locator('[data-goal-page="goal1"]').waitFor()
    await page.locator("[data-goal-link]").click()
    await page.locator('[data-link-option="ST-440"]').click()
    assert.deepEqual(m.calls.at(-1), { method: "POST", path: "/api/digest/todos/goal1/items", body: { item: "ST-440", linked: true } })
    await page.locator('[data-goal-item="ST-440"]').waitFor()
    assert.equal(await page.locator("[data-goal-progress-card] [data-goal-progress]").getAttribute("data-goal-progress"), "33")
    await page.close()
  }

  /* English */
  {
    const { page } = await open({ lang: "en", hash: "/#/calendar" })
    await page.getByText("No calendar connected yet").waitFor()
    assert.equal(await page.locator("[data-calendar-connect]").innerText(), "Connect a calendar")
    await page.locator("[data-calendar-connect]").click()
    await page.waitForURL(/#\/integrations\/ics$/)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("todos.browser: ok")
} finally {
  await browser.close()
}
