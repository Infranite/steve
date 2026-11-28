// Verifies item detail top state cards across all 11 wire-reachable StateKinds in zh-CN and en.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { decisionView, itemView, sessionView, snapshotViews, viewsResponse } from "./fixtures/wire-views.ts"
import { settle } from "./settle.mjs"

const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5196"
const browser = await chromium.launch({
  headless: true,
  ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}),
})

const now = Date.now()
const ws = { id: "default", name: "Acme 研发", description: "", item_prefix: "ST", sort: 0, created: 0 }
const bots = [
  { id: "steve", name: "Steve", role: "工作空间协调者", main: true, harness: "claude-code", model: "claude-opus-4-6", reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "auto", avatar: { color: "#8b7cf6" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
  { id: "backend", name: "后端 Bot", role: "负责后端服务与契约", main: false, harness: "claude-code", model: "claude-opus-4-6", reasoning_effort: "high", rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "ask", avatar: { color: "#14b8a6" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
]
const runtimes = [
  { id: "claude-code", name: "Claude Code", ok: true, version: "2.4.1", path: "/usr/local/bin/claude", models: ["claude-opus-4-6"], default_model: "claude-opus-4-6", note: "", modes: ["plan", "ask", "auto"] },
]
const devices = [
  { id: "hub", name: "Studio Mac mini", node_id: "local", roles: ["Hub"], connected: true, can_configure: true, revision: 1, os: "macOS", version: "0.14.1", status: "online", max_sessions: 8, seen: now, created: 0 },
]

const cases = [
  {
    id: "ST-101",
    kind: "working",
    title: "修复状态卡副标签",
    compact: false,
    sublabel: null,
    item: itemView({ id: "ST-101", title: "修复状态卡副标签", state: { kind: "active" }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    progress: "正在跑 web 单测并检查状态卡展示",
    headline: { "zh-CN": "正在做", en: "Working" },
  },
  {
    id: "ST-102",
    kind: "queued",
    title: "补充消息排队接手",
    compact: false,
    sublabel: { key: "waitingAgent", text: { "zh-CN": "等 Agent 接手", en: "Waiting for an agent" } },
    item: itemView({ id: "ST-102", title: "补充消息排队接手", state: { kind: "active" }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "你的消息在排队", en: "Your message is queued" },
  },
  {
    id: "ST-103",
    kind: "blocked",
    title: "等待前置事项完成",
    compact: false,
    sublabel: null,
    item: itemView({ id: "ST-103", title: "等待前置事项完成", state: { kind: "active" }, waits: ["prerequisite"], deps: ["ST-109"], bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "等前置事项完成", en: "Waiting for prerequisites" },
  },
  {
    id: "ST-104",
    kind: "idle",
    title: "停下待继续的事项",
    compact: false,
    sublabel: null,
    item: itemView({ id: "ST-104", title: "停下待继续的事项", state: { kind: "active" }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "没有在运行", en: "Nothing running" },
    hint: { "zh-CN": "它停下了", en: "It stopped" },
  },
  {
    id: "ST-105",
    kind: "paused",
    title: "已暂停的事项",
    compact: false,
    sublabel: { key: "you", text: { "zh-CN": "该你了", en: "Your move" } },
    item: itemView({ id: "ST-105", title: "已暂停的事项", state: { kind: "paused" }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "已暂停", en: "Paused" },
  },
  {
    id: "ST-106",
    kind: "asking",
    title: "待决定事项",
    compact: null,
    sublabel: null,
    item: itemView({ id: "ST-106", title: "待决定事项", state: { kind: "active" }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "等你决定", en: "Waiting for you" },
  },
  {
    id: "ST-107",
    kind: "dispatching",
    title: "正在分配执行者",
    compact: false,
    sublabel: null,
    item: itemView({ id: "ST-107", title: "正在分配执行者", state: { kind: "active" }, waits: ["dispatching"], created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "Steve 正在安排执行者", en: "Steve is choosing an executor" },
  },
  {
    id: "ST-108",
    kind: "closing",
    title: "收尾验证中",
    compact: false,
    sublabel: null,
    item: itemView({ id: "ST-108", title: "收尾验证中", state: { kind: "closing", request: "complete", waits: ["delivery"] }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "收尾中", en: "Closing out" },
  },
  {
    id: "ST-109",
    kind: "done",
    title: "已完成的事项",
    compact: true,
    sublabel: null,
    item: itemView({ id: "ST-109", title: "已完成的事项", state: { kind: "closed", outcome: "finished", by: "owner" }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "已完成", en: "Done" },
  },
  {
    id: "ST-110",
    kind: "dropped",
    title: "已放弃的事项",
    compact: true,
    sublabel: null,
    item: itemView({ id: "ST-110", title: "已放弃的事项", state: { kind: "closed", outcome: "cancelled", by: "owner" }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "已放弃", en: "Dropped" },
  },
  {
    id: "ST-111",
    kind: "failed",
    title: "失败结束的事项",
    compact: true,
    sublabel: null,
    item: itemView({ id: "ST-111", title: "失败结束的事项", state: { kind: "closed", outcome: "failed", by: "rule" }, bot_id: "backend", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1 }),
    headline: { "zh-CN": "失败结束", en: "Closed as failed" },
  },
]

const items = cases.map((c) => c.item)
const sessions = [
  sessionView({
    id: "ses_101", ws: "default", item_id: "ST-101", no: 1, bot_id: "backend", title: "执行任务",
    harness: "claude-code", model: "claude-opus-4-6", device_id: "hub", node_id: "local",
    status: "running", readonly: false, cwd: "/tmp/acme-api", source: "Steve",
    created: now - 30_000, updated: now,
  }),
  sessionView({
    id: "ses_102", ws: "default", item_id: "ST-102", no: 1, bot_id: "backend", title: "排队会话",
    harness: "claude-code", model: "claude-opus-4-6", device_id: "hub", node_id: "local",
    status: "idle", readonly: false, queued: 1, cwd: "/tmp/acme-api", source: "Steve",
    created: now - 30_000, updated: now,
  }),
]
const decisions = [
  decisionView({
    id: "dec_106", ws: "default", kind: "ask", subject_type: "item", subject_id: "ST-106", item_id: "ST-106",
    question: { kind: "raw", text: "确认继续吗？" }, options: ["继续", "暂停"], status: "open",
    requested_by: "backend", created: now - 10_000, tier: "decide",
  }),
]
const itemById = new Map(cases.map((c) => [c.id, c]))
const shots = process.env.STEVE_TEST_SHOTS
const shotKinds = new Set(["working", "queued", "paused", "idle", "blocked"])
if (shots) await mkdir(shots, { recursive: true })

const errors = []
try {
  for (const locale of ["zh-CN", "en"]) {
    const page = await browser.newPage({ locale, viewport: { width: 1280, height: 860 }, reducedMotion: "reduce" })
    page.setDefaultTimeout(10000)
    page.on("pageerror", (e) => errors.push(`${locale}: ${e.message}`))

    await page.route("**/*", async (route) => {
      const req = route.request(), u = new URL(req.url()), p = u.pathname
      if (u.origin !== base) return route.abort()
      if (p === "/auth/hello") return route.fulfill({ json: { name: "steve", version: "0.14.1", api: 2, login: { token: false, oidc: false } } })
      if (!p.startsWith("/api/")) return route.continue()
      if (p === "/api/shell") return route.fulfill({ json: { version: "0.14.1", web_version: "0.14.1", owner: true, workspace: ws, workspaces: [ws], bots, allBots: bots.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, settings: { onboard: "1" }, devices, runtimes } })
      if (p === "/api/snapshot") return route.fulfill({ json: snapshotViews({ item: items, session: sessions, decision: decisions }) })
      if (p === "/api/views") {
        const kind = u.searchParams.get("kind")
        const list = kind === "item" ? items : kind === "session" ? sessions : kind === "decision" ? decisions : []
        return route.fulfill({ json: viewsResponse(list) })
      }
      if (p === "/api/summary") return route.fulfill({ json: { ws: "default", workspace_name: ws.name, thinking: false, hold: null } })
      if (p === "/api/devices") return route.fulfill({ json: { devices } })
      if (p === "/api/runtimes") return route.fulfill({ json: runtimes })
      if (p === "/api/nodes") return route.fulfill({ json: { nodes: [] } })
      if (p === "/api/node-harnesses") return route.fulfill({ json: { nodes: {} } })
      if (p === "/api/vitals") return route.fulfill({ json: { nodes: {} } })
      if (p === "/api/node-tags") return route.fulfill({ json: { tags: {} } })
      if (p === "/api/bot-teams") return route.fulfill({ json: { default: null, editable: true, teams: [] } })
      if (p.startsWith("/api/items/")) {
        const id = decodeURIComponent(p.slice("/api/items/".length))
        const entry = itemById.get(id)
        if (entry) {
          return route.fulfill({
            json: {
              id,
              source: ["对话", "chat"],
              cwd: "/tmp/acme-api",
              branch: null,
              plan: [],
              goalsList: [{ version: 1, text: entry.title, created: now - 60_000 }],
              planRounds: [],
              timeline: entry.progress ? [{ id: 1, kind: "progress", text: entry.progress, ts: now - 5_000, actor: "backend" }] : [],
              artifacts: [],
              decisions: [],
              dependents: [],
              annotations: [],
            },
          })
        }
      }
      if (p.startsWith("/api/sessions/") && p.endsWith("/events")) return route.fulfill({ json: { events: [], next: null } })
      if (p.startsWith("/api/sessions/") && p.endsWith("/queued")) return route.fulfill({ json: [] })
      if (p.startsWith("/api/permission-policy/sessions/")) return route.fulfill({ json: { policy: "auto", status: "applied", target: "auto", applied: "auto", desired: null, native_id: null, refused: null, unknown: false, released: false } })
      if (p === "/api/kb/uses") return route.fulfill({ json: { hits: [] } })
      if (p === "/api/stream") return route.abort("blockedbyclient")
      if (p.startsWith("/api/stats/")) return route.fulfill({ status: 404, json: {} })
      return route.fulfill({ status: 404, json: {} })
    })

    await page.addInitScript((lang) => {
      localStorage.setItem("steve-lang", lang)
      localStorage.setItem("steve-onboarded", "1")
    }, locale)

    for (const c of cases) {
      await page.goto(`${base}/#/items/${c.id}`)
      const nowCard = page.locator(`[data-now][data-state="${c.kind}"]`)
      await nowCard.waitFor()
      const expectedHeadline = c.headline[locale]
      await page.waitForFunction(
        ({ kind, expected }) => {
          const title = document.querySelector(`[data-now][data-state="${kind}"] [data-state-title]`)
          return title && title.textContent?.trim() === expected
        },
        { kind: c.kind, expected: expectedHeadline },
      )
      assert.equal((await nowCard.locator("[data-state-title]").innerText()).trim(), expectedHeadline, `${locale} ${c.kind}: headline`)

      const sublabel = nowCard.locator("[data-state-sublabel]")
      if (c.sublabel) {
        assert.equal(await sublabel.count(), 1, `${locale} ${c.kind}: expects 1 sublabel`)
        assert.equal(await sublabel.getAttribute("data-state-sublabel"), c.sublabel.key, `${locale} ${c.kind}: sublabel key`)
        assert.equal((await sublabel.innerText()).trim(), c.sublabel.text[locale], `${locale} ${c.kind}: sublabel text`)
      } else {
        assert.equal(await sublabel.count(), 0, `${locale} ${c.kind}: expects no sublabel`)
      }

      const cardText = await nowCard.innerText()
      assert.doesNotMatch(cardText, /等 agent 处理|With the agents/, `${locale} ${c.kind}: no misleading sublabel text`)
      if (c.hint) assert.ok(cardText.includes(c.hint[locale]), `${locale} ${c.kind}: shows idleHint`)

      if (c.compact !== null) {
        const classes = await nowCard.evaluate((el) => el.className.split(/\s+/))
        if (c.compact) {
          assert.ok(classes.includes("p-4"), `${locale} ${c.kind}: closed card uses compact p-4`)
        } else {
          assert.ok(classes.includes("p-5"), `${locale} ${c.kind}: open card uses p-5`)
          assert.equal(classes.includes("p-4"), false, `${locale} ${c.kind}: open card must not use compact p-4`)
        }
      }

      if (shots && shotKinds.has(c.kind)) {
        await settle(page)
        await page.screenshot({ path: `${shots}/item-state-${locale}-${c.kind}.png`, fullPage: true })
      }
    }

    await page.close()
  }

  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, locales: ["zh-CN", "en"], states: cases.map((c) => c.kind) }))
} finally {
  await browser.close()
}
