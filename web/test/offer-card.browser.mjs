// Browser smoke for OFFER card placement preview, Bot switching, and zero-eligible fallback selection.
import assert from "node:assert/strict"
import { createServer } from "node:http"
import { pageResponse, patchFrame, snapshotViews, viewsResponse, messageView, itemView } from "./fixtures/wire-views.ts"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"

const sseClients = new Set()
let sseHead = 1
const publish = (o) => {
  const n = ++sseHead
  const chunk = `id: fixture:${n}\ndata: ${JSON.stringify(o)}\n\n`
  for (const c of sseClients) c.write(chunk)
}
const sse = createServer((req, res) => {
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" })
  res.write(": ok\n\n")
  sseClients.add(res)
  req.on("close", () => sseClients.delete(res))
})
await new Promise((r) => sse.listen(0, "127.0.0.1", r))
const ssePort = sse.address().port

const now = Date.now()
const ws = { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }
const bots = [
  { id: "steve", name: "Steve", role: "工作空间协调者", main: true, harness: "claude-code", model: null, reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "auto", avatar: { color: "#8b7cf6" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
  { id: "dev", name: "开发", role: "前端与后端代码实现", main: false, harness: "codex", model: null, reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "auto", avatar: { color: "#14b8a6" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
  { id: "bot_doc", name: "文档", role: "写和整理文档、调研笔记", main: false, harness: "antigravity", model: null, reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "auto", avatar: { color: "#60a5fa" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
  { id: "bot_off", name: "停用测试", role: "已停用 Bot", main: false, harness: "codex", model: null, reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "auto", avatar: { color: "#6c7a89" }, default_project: null, prompt: null, device: null, enabled: false, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
]

function initialMessages() {
  return [
    messageView({ id: "u_1", ws: "default", author: "me", text: "页面按钮间距不太齐", created: now - 60_000 }),
    messageView({
      id: "m_offer_1",
      ws: "default",
      author: "steve",
      text: "需要调整前端样式。",
      created: now - 50_000,
      meta: {
        origin: "u_1",
        offer: {
          title: "优化预览样式",
          goal: "统一按钮间距",
          project: null,
          tags: ["t_fe"],
          bot: "dev",
          reason: "steve_selected",
          why: "开发 Bot 负责前端样式调整且当前负载低",
          state: "open",
        },
      },
    }),
    messageView({ id: "u_2", ws: "default", author: "me", text: "派发说明也要更新", created: now - 40_000 }),
    messageView({
      id: "m_offer_2",
      ws: "default",
      author: "steve",
      text: "可以补齐说明文档。",
      created: now - 30_000,
      meta: {
        origin: "u_2",
        offer: {
          title: "补齐使用文档",
          goal: "整理派发规则说明",
          project: null,
          tags: [],
          bot: "dev",
          reason: "only_eligible",
          state: "open",
        },
      },
    }),
    messageView({ id: "u_3", ws: "default", author: "me", text: "紧急修复一下", created: now - 20_000 }),
    messageView({
      id: "m_offer_3",
      ws: "default",
      author: "steve",
      text: "建议交给停用测试 Bot 处理。",
      created: now - 10_000,
      meta: {
        origin: "u_3",
        offer: {
          title: "紧急热修复",
          goal: "Steve 选择了已停用的 Bot",
          project: null,
          tags: [],
          bot: "bot_off",
          why: "历史上修过同类问题",
          rejected: "bot",
          state: "open",
        },
      },
    }),
  ]
}

const runtimes = [
  { id: "codex", name: "Codex", ok: true, version: "", path: "", models: [], note: "", modes: ["auto"] },
  { id: "antigravity", name: "Antigravity", ok: true, version: "", path: "", models: [], note: "", modes: ["auto"] },
  { id: "claude-code", name: "Claude Code", ok: true, version: "", path: "", models: [], note: "", modes: ["auto"] },
]

const errors = []
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 860 }, reducedMotion: "reduce" })
    page.setDefaultTimeout(10_000)
    page.on("pageerror", (e) => errors.push(e.message))
    page.on("console", (m) => { if (m.type() === "error" && m.text().includes("Base UI:")) errors.push(m.text()) })

    let messages = initialMessages()
    const items = []
    const offerCalls = []
    let failNextOverride = true
    let rev = 1
    sseHead = rev

    await page.route("**/*", async (route) => {
      const u = new URL(route.request().url())
      if (u.origin !== base) return route.abort()
      if (u.pathname === "/auth/hello") return route.fulfill({ json: { name: "steve", version: "fixture", api: 2, login: { token: false, oidc: false } } })
      if (!u.pathname.startsWith("/api/")) return route.continue()
      const method = route.request().method(), p = u.pathname
      if (p === "/api/stream") return route.continue({ url: `http://127.0.0.1:${ssePort}${u.pathname}${u.search}` })
      if (p === "/api/shell") {
        return route.fulfill({ json: {
          modules: { feishuRegistration: false },
          version: "fixture", web_version: "fixture", owner: true, workspace: ws, workspaces: [ws],
          bots, allBots: bots.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })),
          projects: [], tags: [], tagUses: {}, settings: { onboard: "1" }, devices: [],
          runtimes,
        } })
      }
      if (p === "/api/runtimes") return route.fulfill({ json: runtimes })
      if (p === "/api/tags") return route.fulfill({ json: { tags: [], uses: {} } })
      if (p === "/api/devices") return route.fulfill({ json: { devices: [] } })
      if (p === "/api/snapshot") {
        return route.fulfill({ json: snapshotViews({ item: items, session: [], decision: [], message: pageResponse(messages, rev, null) }, { rev, cursor: `fixture:${rev}` }) })
      }
      if (p === "/api/summary") return route.fulfill({ json: { ws: "default", workspace_name: "默认", thinking: false, hold: null } })
      if (p === "/api/views") return route.fulfill({ json: viewsResponse([], rev) })
      if (p === "/api/views/page") return route.fulfill({ json: pageResponse([], rev, null) })
      if (p === "/api/notifications") return route.fulfill({ json: [] })
      if (p === "/api/me") return route.fulfill({ json: { user: { id: "u1", name: "Owner", email: null, role: "owner" }, version: "fixture", web_version: "fixture" } })
      if (p === "/api/conversations/channels") return route.fulfill({ json: { channels: [] } })
      if (p === "/api/conversations") return route.fulfill({ json: { conversations: [], has_more: false, next_offset: null, total: 0 } })
      if (p === "/api/plugins/themes") return route.fulfill({ json: { themes: [] } })
      if (p.startsWith("/api/stats/")) return route.fulfill({ status: 404, json: {} })
      const match = /^\/api\/thread\/([^/]+)\/offer$/.exec(p)
      if (match && method === "POST") {
        const msgId = decodeURIComponent(match[1])
        const body = route.request().postDataJSON()
        offerCalls.push({ id: msgId, body })
        if (msgId === "m_offer_2" && body.bot === "bot_doc" && failNextOverride) {
          failNextOverride = false
          return route.fulfill({
            status: 409,
            json: {
              error: "该 Bot 不在 Team 的启用成员中。",
              placement: { reason: "outside_team", rejected: [{ bot: "bot_doc", reason: "outside_team" }] },
            },
          })
        }
        const itemId = `ST-${100 + offerCalls.length}`
        const botId = body.bot ?? "dev"
        rev += 1
        const item = itemView({ id: itemId, title: `Offer ${msgId}`, bot_id: botId, state: { kind: "active" }, created: now, updated: now })
        items.push(item)
        let updatedMsg = null
        messages = messages.map((m) => {
          if (m.id !== msgId) return m
          updatedMsg = { ...m, meta: { ...m.meta, offer: { ...m.meta.offer, state: "taken", item: itemId, bot: botId } } }
          return updatedMsg
        })
        if (updatedMsg) {
          publish(patchFrame([
            { type: "item", op: "upsert", id: itemId, view: item },
            { type: "message", op: "upsert", id: msgId, view: updatedMsg },
          ], rev))
        }
        return route.fulfill({ json: { message_id: `reply_${msgId}`, item: { id: itemId } } })
      }
      return route.fulfill({ status: 200, json: {} })
    })

    await page.addInitScript(() => {
      localStorage.setItem("steve-lang", "zh-CN")
      localStorage.setItem("steve-chat-rail", "0")
      localStorage.setItem("steve-onboarded", "1")
    })
    await page.goto(base + "/")

    // 1. Steve's chosen Bot and rationale show on m_offer_1; taking without override sends { take: true }.
    const bot1 = page.locator('[data-offer-bot="m_offer_1"]')
    await bot1.waitFor()
    assert.match(await bot1.innerText(), /Steve 推荐 · 开发 Bot/)
    assert.match(await page.locator('[data-offer-why="m_offer_1"]').innerText(), /开发 Bot 负责前端样式调整且当前负载低/)
    await bot1.click()
    const listbox1 = page.getByRole("listbox")
    await listbox1.waitFor()
    assert.match(await listbox1.innerText(), /Steve 选择了这个 Bot/)
    assert.equal(await listbox1.getByRole("option", { name: /停用测试/ }).count(), 0, "disabled bots must not be selectable")
    await page.keyboard.press("Escape")
    await listbox1.waitFor({ state: "detached" })
    const post1 = page.waitForResponse((r) => r.url().endsWith("/api/thread/m_offer_1/offer") && r.request().method() === "POST")
    await page.locator('[data-offer="m_offer_1"] [data-offer-take]').click()
    await post1
    await page.locator('[data-offer="m_offer_1"]').waitFor({ state: "detached" })
    assert.equal(offerCalls.length, 1)
    assert.deepEqual(offerCalls[0], { id: "m_offer_1", body: { take: true } })

    // 2. Switching Bot on m_offer_2 sends explicit bot override; a 409 placement refusal keeps the offer open for retry.
    const bot2 = page.locator('[data-offer-bot="m_offer_2"]')
    await bot2.waitFor()
    assert.match(await bot2.innerText(), /唯一合格 · 开发 Bot/)
    await bot2.click()
    const listbox2 = page.getByRole("listbox")
    await listbox2.waitFor()
    assert.match(await listbox2.innerText(), /当前唯一合格的 Bot/)
    await listbox2.getByRole("option", { name: /文档 Bot/ }).click()
    await listbox2.waitFor({ state: "detached" })
    assert.match(await bot2.innerText(), /文档 Bot/)
    const post2Blocked = page.waitForResponse((r) => r.url().endsWith("/api/thread/m_offer_2/offer") && r.request().method() === "POST")
    await page.locator('[data-offer="m_offer_2"] [data-offer-take]').click()
    assert.equal((await post2Blocked).status(), 409)
    await page.locator('[data-offer="m_offer_2"]').waitFor({ state: "visible" })
    const post2 = page.waitForResponse((r) => r.url().endsWith("/api/thread/m_offer_2/offer") && r.request().method() === "POST")
    await page.locator('[data-offer="m_offer_2"] [data-offer-take]').click()
    assert.equal((await post2).status(), 200)
    await page.locator('[data-offer="m_offer_2"]').waitFor({ state: "detached" })
    assert.equal(offerCalls.length, 3)
    assert.deepEqual(offerCalls[1], { id: "m_offer_2", body: { take: true, bot: "bot_doc" } })
    assert.deepEqual(offerCalls[2], { id: "m_offer_2", body: { take: true, bot: "bot_doc" } })

    // 3. Rejected Steve choice surfaces the real rejection reason, blocks take until owner switches Bot.
    const bot3 = page.locator('[data-offer-bot="m_offer_3"]')
    await bot3.waitFor()
    assert.match(await bot3.innerText(), /停用测试 Bot（不可用）/)
    assert.match(await page.locator('[data-offer="m_offer_3"] [data-offer-rejected="bot"]').innerText(), /Bot 已停用或不再属于当前工作空间/)
    assert.equal(await page.locator('[data-offer="m_offer_3"] [data-offer-take]').isDisabled(), true)
    await bot3.click()
    const listbox3 = page.getByRole("listbox")
    await listbox3.waitFor()
    assert.match(await listbox3.innerText(), /请在此改选其他执行 Bot/)
    await listbox3.getByRole("option", { name: /Steve · 协调者/ }).click()
    await listbox3.waitFor({ state: "detached" })
    const post3 = page.waitForResponse((r) => r.url().endsWith("/api/thread/m_offer_3/offer") && r.request().method() === "POST")
    await page.locator('[data-offer="m_offer_3"] [data-offer-take]').click()
    await post3
    await page.locator('[data-offer="m_offer_3"]').waitFor({ state: "detached" })
    assert.equal(offerCalls.length, 4)
    assert.deepEqual(offerCalls[3], { id: "m_offer_3", body: { take: true, bot: "steve" } })

    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `no horizontal overflow at ${width}px`)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["offer preview reason", "disabled bots excluded", "default auto take", "switch target bot override", "unassigned preview fallback", "1280/390px"] }))
} finally {
  for (const c of sseClients) c.destroy()
  sse.close()
  await browser.close()
}
