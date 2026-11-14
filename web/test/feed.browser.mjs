// Full app with isolated API fixtures, never a real Hub write acceptance.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5215", shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const now = Date.now()
const content = { title: "", markdown: "## 阅读而不是待处理\n\n这是一条自动生成的完整汇报，包含生活、兴趣与日常进展。\n\n- 书单已整理\n- 本周旅行安排已核对\n\n> 阅读不会发送聊天或解决决定。\n\n" + "长文段落：保持阅读位置，稍后再看新内容。\n\n".repeat(35) + "https://example.com/" + "very-long-reference-".repeat(35), fields: [["覆盖时间", "过去一天"]], actions: [["Unsafe action", "javascript:alert(1)"]], references: [["真实格式来源示例", "https://example.com/notes"], ["ST-42", "#/items/ST-42"]] }
const entry = (seq, extra = {}) => ({ id: "entry-" + seq, seq, kind: seq % 3 ? "report" : "completion", title: seq === 80 ? "周六早间汇报 · 生活、兴趣与进展" : seq % 3 ? `阅读摘录与今日汇报 ${seq}` : `你关注的事项已完成 ${seq}`, excerpt: "已整理本周阅读与旅行资料；新进展集中在这里，不会打断对话。", body: null, published_at: now - (80 - seq) * 7200000, read_at: null, purged: false, source: { type: seq % 3 ? "flow" : "notification", id: "source-" + seq, run_id: seq % 3 ? "run-" + seq : null, node_id: null, event_id: null }, ...extra })
const state = { version: "fixture", web_version: "fixture", owner: false, workspace: { id: "ws_1", name: "Reading room", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [{ id: "ws_1", name: "Reading room", description: "", item_prefix: "ST", sort: 0, created: 0 }, { id: "ws_2", name: "Second workspace", description: "", item_prefix: "ST", sort: 1, created: 0 }], items: [], decisions: [], recent: [], bots: [], allBots: [], projects: [], tags: [], tagUses: {}, thread: [], settings: { theme: "light" }, sessions: [], devices: [], runtimes: [], notifications: [], notifySeen: 0 }
const errors = []
async function open(options = {}) {
  const model = { entries: Array.from({ length: 80 }, (_, i) => entry(80 - i)), otherEntries: [entry(901, { title: "Second workspace report" })], fail: false, readFail: false, moreExpired: false, detailFail: false, workspace: "ws_1", ...options }
  const theme = options.theme ?? "light", width = options.width ?? 1440, lang = options.lang ?? "zh-CN"
  const page = await browser.newPage({ viewport: { width, height: 1000 }, colorScheme: theme, reducedMotion: "reduce", locale: lang })
  page.setDefaultTimeout(12000)
  page.on("pageerror", (error) => errors.push(error.message))
  await page.addInitScript(({ theme, lang }) => { localStorage.setItem("steve-lang", lang); localStorage.setItem("steve-onboarded", "1"); document.addEventListener("DOMContentLoaded", () => document.documentElement.classList.toggle("dark", theme === "dark")) }, { theme, lang })
  const calls = [], snapshots = new Map(), cursors = new Map()
  let releaseDetail, detailRequested
  const detailGate = new Promise((resolve) => { releaseDetail = resolve })
  const requested = new Promise((resolve) => { detailRequested = resolve })
  await page.route((url) => url.pathname === "/auth/hello" || url.pathname.startsWith("/api/"), async (route) => {
    const req = route.request(), url = new URL(req.url()), path = url.pathname, method = req.method(), workspace = req.headers()["x-steve-ws"]
    const body = req.postData() ? req.postDataJSON() : null
    calls.push({ method, path, workspace, body, query: url.search, origin: url.origin })
    const json = (value, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(value) }).catch(() => {})
    const fail = (status = 503) => json({ error: "fixture unavailable" }, status)
    if (path === "/auth/hello") return json({ name: "steve", api: 1, version: "fixture", login: { token: true, oidc: false } })
    if (path === "/api/state") return json({ ...state, workspace: state.workspaces.find((ws) => ws.id === model.workspace), settings: { theme } })
    if (path === "/api/me") return json({ user: { id: "reader", name: "Morgan", email: null, role: "member" }, version: "fixture", web_version: "fixture" })
    if (path === "/api/plugins/themes") return json({ themes: [] })
    if (path === "/api/onboard") return json({ state: "done" })
    if (path === "/api/workspaces/ws_2/switch") { model.workspace = "ws_2"; return json({ ok: true }) }
    if (path === "/api/me/feed") {
      if (model.fail) return fail()
      const params = url.searchParams, filter = { kind: params.get("kind"), source: params.get("source"), unread: params.get("unread") === "true" }
      const matches = (entry) => (!filter.kind || entry.kind === filter.kind) && (!filter.source || entry.source.type === filter.source)
      const all = (workspace === "ws_2" ? model.otherEntries : model.entries).filter(matches)
      const eligible = all.filter((entry) => !filter.unread || entry.read_at === null)
      let boundary = eligible[0]?.seq ?? 0, offset = 0
      if (params.has("cursor")) {
        if (model.moreExpired) return fail(400)
        const cursor = cursors.get(params.get("cursor")); assert.ok(cursor)
        boundary = cursor.boundary; offset = cursor.offset
      }
      const snapshot = JSON.stringify({ boundary, workspace, filter }); snapshots.set(snapshot, { boundary, workspace, filter })
      const pool = eligible.filter((entry) => entry.seq <= boundary), limit = Number(params.get("limit") ?? 50)
      const entries = pool.slice(offset, offset + limit)
      const cursor = offset + limit < pool.length ? JSON.stringify({ boundary, offset: offset + limit }) : null
      if (cursor) cursors.set(cursor, { boundary, offset: offset + limit })
      return json({ entries: entries.map((entry) => ({ ...entry, body: null })), next_cursor: cursor, snapshot, unread_count: all.filter((entry) => entry.read_at === null).length })
    }
    if (path === "/api/me/feed/read") {
      if (model.readFail) return fail()
      const entries = workspace === "ws_2" ? model.otherEntries : model.entries
      let targets
      if (body.ids) targets = entries.filter((entry) => body.ids.includes(entry.id))
      else {
        const snapshot = snapshots.get(body.snapshot); assert.ok(snapshot); assert.equal(snapshot.workspace, workspace)
        assert.equal(body.kind ?? null, snapshot.filter.kind); assert.equal(body.source ?? null, snapshot.filter.source); assert.equal(body.unread ?? false, snapshot.filter.unread)
        targets = entries.filter((entry) => entry.seq <= snapshot.boundary && (!body.kind || entry.kind === body.kind) && (!body.source || entry.source.type === body.source) && (!body.unread || entry.read_at === null))
      }
      const updated = targets.filter((entry) => entry.read_at === null).length
      targets.forEach((entry) => { entry.read_at ??= Date.now() })
      return json({ updated })
    }
    if (path.startsWith("/api/me/feed/")) {
      if (model.detailFail) return fail()
      const entry = (workspace === "ws_2" ? model.otherEntries : model.entries).find((entry) => entry.id === decodeURIComponent(path.split("/").at(-1)))
      if (model.delayDetail && workspace === "ws_1") { detailRequested(); await detailGate }
      return entry ? json({ ...entry, body: entry.purged ? null : content }) : fail(404)
    }
    return json({ error: "fixture route absent" }, 404)
  })
  await page.goto(base + (options.hash ?? "/#/feed"))
  await page.locator("[data-feed]").waitFor()
  return { page, model, calls, requested, releaseDetail }
}
const fits = async (page) => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
const shot = async (page, name) => { await fits(page); if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true }) }
try {
  for (const theme of ["light", "dark"]) for (const width of [1440, 390]) {
    const { page, calls } = await open({ theme, width })
    await page.locator('[data-feed-entry="entry-80"]').waitFor()
    assert.equal(await page.locator("[data-feed-entry]").count(), 50)
    assert.equal(calls.filter((call) => call.method !== "GET").length, 0)
    if (width === 390) { assert.equal(await page.locator("[data-mobile-tabs] a").count(), 5); assert.equal(await page.locator('[data-tab="feed"]').getAttribute("aria-current"), "page") }
    else assert.equal(await page.locator('[data-nav="feed"]').getAttribute("aria-current"), "page")
    await shot(page, `feed-${width}-${theme}`)
    await page.locator('[data-feed-entry="entry-80"] [data-feed-expand]').click()
    await page.getByText("阅读而不是待处理", { exact: true }).waitFor()
    assert.equal(await page.getByRole("link", { name: "Unsafe action" }).count(), 0)
    assert.equal(await page.getByRole("link", { name: "查看事项", exact: true }).getAttribute("href"), "#/items/ST-42")
    assert.equal(calls.filter((call) => call.method !== "GET").length, 0, "expanding does not mark read")
    await shot(page, `feed-long-${width}-${theme}`)
    await page.close()
  }
  {
    const { page, model, calls } = await open({ fail: true })
    await page.locator("[data-feed]").getByRole("alert").waitFor(); model.fail = false
    await page.getByRole("button", { name: "重试", exact: true }).click()
    await page.locator('[data-feed-entry="entry-80"]').waitFor()
    await page.locator("[data-feed-more]").click()
    await page.locator('[data-feed-entry="entry-1"]').waitFor()
    assert.equal(await page.locator("[data-feed-entry]").count(), 80)
    model.detailFail = true
    await page.locator('[data-feed-entry="entry-1"] [data-feed-expand]').click()
    await page.locator('[data-feed-entry="entry-1"]').getByRole("alert").waitFor()
    await shot(page, "feed-error-retry")
    model.detailFail = false
    await page.locator('[data-feed-entry="entry-1"]').getByRole("button", { name: "重试" }).click()
    await page.locator('[data-feed-entry="entry-1"]').getByText("阅读而不是待处理", { exact: true }).waitFor()
    model.readFail = true
    await page.locator('[data-feed-entry="entry-1"] [data-feed-read-one]').click()
    await page.locator('[data-feed] > [role="alert"]').waitFor()
    assert.equal(await page.locator('[data-feed-entry="entry-1"]').getAttribute("data-feed-read"), "false")
    model.readFail = false
    await page.locator('[data-feed] > [role="alert"]').getByRole("button", { name: "重试" }).click()
    await page.locator('[data-feed-entry="entry-1"][data-feed-read="true"]').waitFor()
    assert.ok(calls.filter((call) => call.method === "POST").every((call) => call.workspace === "ws_1"))
    await page.close()
  }
  {
    const { page, model, calls } = await open()
    await page.locator('[data-feed-entry="entry-80"]').waitFor()
    await page.locator("[data-feed-more]").click(); await page.locator('[data-feed-entry="entry-1"]').waitFor()
    await page.locator('[data-feed-entry="entry-20"]').scrollIntoViewIfNeeded()
    const before = await page.locator("[data-feed]").evaluate((el) => el.parentElement.parentElement.parentElement.scrollTop)
    model.entries.unshift(entry(81, { title: "Concurrent new report" }))
    await page.evaluate(() => window.dispatchEvent(new Event("focus")))
    await page.locator("[data-feed-new]").waitFor()
    const after = await page.locator("[data-feed]").evaluate((el) => el.parentElement.parentElement.parentElement.scrollTop)
    assert.equal(before, after, "new content does not displace reading")
    await shot(page, "feed-new-content")
    await page.locator("[data-feed-select-all]").check()
    model.readFail = true
    await page.locator("[data-feed-read-selected]").click()
    await page.locator("[data-feed]").getByRole("alert").waitFor()
    assert.ok(await page.locator("[data-feed-select-all]").isChecked(), "failed write retains selection")
    model.readFail = false
    await page.locator("[data-feed]").getByRole("alert").getByRole("button", { name: "重试" }).click()
    await page.locator('[data-feed-entry="entry-80"][data-feed-read="true"]').waitFor()
    assert.equal(model.entries[0].read_at, null, "unloaded concurrent entry stays unread")
    assert.equal(calls.findLast((call) => call.method === "POST").body.ids.length, 80)
    await page.close()
  }
  {
    const { page, model, calls } = await open({ hash: "/#/feed?kind=completion" })
    await page.locator('[data-feed-entry="entry-78"]').waitFor()
    assert.ok((await page.locator("[data-feed-entry]").allTextContents()).every((text) => text.includes("完成通知")))
    model.entries.unshift(entry(81, { kind: "completion", source: { ...entry(81).source, type: "notification" } }))
    await page.locator("[data-feed-read-all]").click()
    await shot(page, "feed-snapshot-confirm")
    await page.locator("[data-feed-confirm-read]").click()
    await page.locator('[data-feed-entry="entry-78"][data-feed-read="true"]').waitFor()
    assert.equal(model.entries[0].read_at, null)
    assert.ok(model.entries.filter((entry) => entry.kind === "report").every((entry) => entry.read_at === null))
    assert.equal(calls.findLast((call) => call.method === "POST").body.kind, "completion")
    await page.locator("[data-feed-refresh]").click()
    await page.locator('[data-feed-entry="entry-81"]').waitFor()
    await page.reload(); await page.locator('[data-feed-entry="entry-78"][data-feed-read="true"]').waitFor()
    await page.close()
  }
  {
    const { page, calls } = await open({ entries: [entry(80, { read_at: 1 }), entry(79), entry(78)] })
    await page.locator('[data-feed-entry="entry-80"]').waitFor()
    await page.getByLabel("来源", { exact: true }).click()
    await page.getByRole("option", { name: "自动化", exact: true }).click()
    await page.getByRole("tab", { name: "未读", exact: true }).click()
    await page.locator('[data-feed-entry="entry-79"]').waitFor()
    assert.equal(await page.locator("[data-feed-entry]").count(), 1)
    assert.match(calls.findLast((call) => call.path === "/api/me/feed").query, /source=flow/)
    assert.match(calls.findLast((call) => call.path === "/api/me/feed").query, /unread=true/)
    await page.locator('[data-feed-entry="entry-79"] [data-feed-expand]').click()
    await page.locator('[data-feed-entry="entry-79"] [data-feed-read-one]').click()
    await page.locator('[data-feed-entry="entry-79"][data-feed-read="true"]').waitFor()
    await page.locator("[data-feed-refresh]").click()
    await page.getByText("未读都看完了", { exact: true }).waitFor()
    await page.close()
  }
  {
    const { page, model, calls } = await open({ moreExpired: true })
    await page.locator('[data-feed-entry="entry-80"]').waitFor()
    await page.locator("[data-feed-more]").click()
    await page.getByRole("button", { name: "从头刷新", exact: true }).waitFor()
    assert.equal(await page.locator("[data-feed-entry]").count(), 50)
    model.moreExpired = false
    await page.getByRole("button", { name: "从头刷新", exact: true }).click()
    await page.waitForFunction(() => !document.querySelector('[data-feed] [role="alert"]'))
    assert.ok(!calls.findLast((call) => call.path === "/api/me/feed").query.includes("cursor="))
    await page.close()
  }
  {
    const { page, model, requested, releaseDetail, calls } = await open({ delayDetail: true })
    await page.locator('[data-feed-entry="entry-80"] [data-feed-expand]').click(); await requested
    await page.locator("[data-ws-switch]").click(); await page.locator('[data-ws-opt="Second workspace"]').click()
    await page.waitForFunction(() => location.hash === "#/")
    await page.goto(base + "/#/feed")
    await page.getByText("Second workspace report", { exact: true }).waitFor()
    releaseDetail()
    assert.equal(await page.locator('[data-feed-entry="entry-80"]').count(), 0)
    assert.ok(calls.filter((call) => call.path === "/api/me/feed").some((call) => call.workspace === "ws_2"))
    assert.equal(model.workspace, "ws_2")
    await shot(page, "feed-workspace-isolation")
    await page.close()
  }
  {
    const { page } = await open({ entries: [entry(80, { purged: true, excerpt: "", title: "" })], lang: "en", theme: "dark", width: 390 })
    await page.getByText("Untitled", { exact: true }).click()
    await page.locator("[data-feed-body]").getByText("This body was cleared by retention or source cleanup.", { exact: true }).waitFor()
    await shot(page, "feed-purged-390-dark-en")
    await page.close()
  }
  {
    const entries = Array.from({ length: 231 }, (_, index) => entry(231 - index))
    const { page, calls } = await open({ entries })
    await page.locator('[data-feed-entry="entry-231"]').waitFor()
    for (let expected = 100; expected <= 231; expected = expected === 200 ? 231 : expected + 50) {
      await page.locator("[data-feed-more]").click()
      await page.waitForFunction((expected) => document.querySelectorAll("[data-feed-entry]").length === expected, expected)
      if (expected === 231) break
    }
    await page.locator("[data-feed-select-all]").check()
    assert.equal(await page.locator('[data-feed-entry] input:checked').count(), 100)
    assert.ok(await page.locator('[data-feed-entry="entry-1"] input').isDisabled())
    assert.ok(await page.locator('[data-feed-entry="entry-231"] input').isEnabled(), "selected rows remain cancellable")
    await page.locator('[data-feed-entry="entry-231"] input').uncheck()
    assert.ok(await page.locator('[data-feed-entry="entry-1"] input').isEnabled())
    await page.locator('[data-feed-entry="entry-1"] input').check()
    await shot(page, "feed-selection-limit-231")
    await page.locator("[data-feed-read-selected]").click()
    await page.locator('[data-feed-entry="entry-1"][data-feed-read="true"]').waitFor()
    assert.equal(calls.findLast((call) => call.method === "POST").body.ids.length, 100)
    assert.ok(!calls.findLast((call) => call.method === "POST").body.ids.includes("entry-231"))
    await page.close()
  }
  {
    const { page } = await open({ entries: [], lang: "en" })
    await page.getByText("Nothing here yet", { exact: true }).waitFor()
    await shot(page, "feed-empty-en")
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("Feed browser fixtures: passed, 1440/390 light/dark; long body, explicit reads, errors, pages, snapshot concurrency, expired cursor, source/unread filters, 231-row selection cap, workspace late response, purged, English. Not real Hub write acceptance.")
} finally { await browser.close() }
