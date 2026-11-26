// Regression test for Issue #580:
// 1) Local Node Harness tab renders exactly 1 harness list (no duplicate RuntimeSetupPanel).
// 2) AddHarness uses typed controls (0 raw JSON <textarea>) and posts a strongly-typed PropertiesPatch.
// 3) Bot named "后端 Bot" never renders "Bot Bot"; Bot Settings splits into 3 sub-tabs with sticky Save and 0 zh-CN leaks.
// 4) Item detail renders compact 2-line session cards without single-char truncation, folds approval/nativeId, and deduplicates pending cards.
// 5) Session detail prints Node name once, moves NativeIdLine into collapsible Runtime drawer, and shows refusal reason inline.
import assert from "node:assert/strict"
import { decisionView, itemView as makeItemView, sessionView as makeSessionView, snapshotViews, viewsResponse } from "./fixtures/wire-views.ts"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5195"
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })

const now = Date.now()
const ws = { id: "default", name: "Acme 研发", description: "", item_prefix: "ST", sort: 0, created: 0 }
const bots = [
  { id: "steve", name: "Steve", role: "工作空间协调者", main: true, harness: "claude-code", model: "claude-opus-4-6", reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "auto", avatar: { color: "#8b7cf6" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
  { id: "backend", name: "后端 Bot", role: "负责后端服务与契约", main: false, harness: "claude-code", model: "claude-opus-4-6", reasoning_effort: "high", rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "ask", avatar: { color: "#14b8a6" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
]
const runtimes = [
  { id: "claude-code", name: "Claude Code", ok: true, version: "2.4.1", path: "/usr/local/bin/claude", models: ["claude-opus-4-6", "claude-sonnet-4-6"], default_model: "claude-opus-4-6", note: "", modes: ["plan", "ask", "accept_edits", "auto", "full_access"] },
  { id: "codex", name: "Codex", ok: false, version: "", path: "", models: [], note: "", modes: ["plan", "ask", "auto"], setup: { state: "not_installed", can_install: true, components: [{ id: "cli", name: "Codex CLI", package: "@openai/codex", status: "missing" }] } },
]
const devices = [
  { id: "hub", name: "Studio Mac mini", node_id: "local", roles: ["Hub"], connected: true, can_configure: true, revision: 1, os: "macOS", version: "0.12.0", status: "online", max_sessions: 8, seen: now, created: 0 },
]
const itemView = makeItemView({
  id: "ST-42", ws: "default", no: 42, title: "为 /auth/token 补充限流",
  state: { kind: "wait", detail: "合并 PR #128 (acme/acme-api) ？" },
  bot_id: "backend", priority: "高", source: "对话", created: now - 60_000, updated: now, goals: 1, detail_stamp: 1,
})
const sessionView = makeSessionView({
  id: "ses_1", ws: "default", item_id: "ST-42", no: 1, bot_id: "backend", title: "实现令牌桶限流",
  harness: "claude-code", model: "claude-opus-4-6", device_id: "hub", node_id: "local",
  status: "running", readonly: false, harness_session_id: "sess_9f2a1b3c4d", cwd: "/tmp/acme-api",
  source: "Steve", used: 34000, size: 100000, created: now - 30_000, updated: now,
})
const waitDecision = decisionView({
  id: "dec_pr_128", ws: "default", kind: "approve", subject_type: "pr_merge", subject_id: "128", item_id: "ST-42", session_id: "ses_1",
  question: { kind: "raw", text: "合并 PR #128 (acme/acme-api) ？" }, options: ["yes", "no"], status: "open",
  requested_by: "steve", created: now - 10_000, tier: "decide",
})

const errors = []
const addedHarnesses = []
try {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1280, height: 860 }, reducedMotion: "reduce" })
  page.setDefaultTimeout(10000)
  page.on("pageerror", (e) => errors.push(e.message))

  await page.route("**/*", async (route) => {
    const req = route.request(), u = new URL(req.url()), p = u.pathname, method = req.method()
    if (u.origin !== base) return route.abort()
    if (p === "/auth/hello") return route.fulfill({ json: { name: "steve", version: "0.12.0", api: 2, login: { token: false, oidc: false } } })
    if (!p.startsWith("/api/")) return route.continue()
    if (p === "/api/shell") return route.fulfill({ json: { version: "0.12.0", web_version: "0.12.0", owner: true, workspace: ws, workspaces: [ws], bots, allBots: bots.map(b => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, settings: { onboard: "1" }, devices, runtimes } })
    if (p === "/api/snapshot") return route.fulfill({ json: snapshotViews({ item: [itemView], session: [sessionView], decision: [waitDecision] }) })
    if (p === "/api/views") {
      const kind = u.searchParams.get("kind")
      const list = kind === "item" ? [itemView] : kind === "session" ? [sessionView] : kind === "decision" ? [waitDecision] : []
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
    if (p === "/api/system/harnesses" && method === "POST") {
      addedHarnesses.push(req.postDataJSON())
      return route.fulfill({ json: { ok: true } })
    }
    if (p === "/api/system/models") return route.fulfill({ json: { harness: "claude-code", name: "Claude Code", source: "fixture", default: "claude-opus-4-6", families: ["Claude"], models: [{ id: "claude-opus-4-6", label: "Claude Opus 4.6", family: "Claude", via: null, note: null, efforts: [] }] } })
    if (p === "/api/items/ST-42") return route.fulfill({ json: { id: "ST-42", source: ["对话", "chat"], cwd: "/tmp/acme-api", branch: "steve/st-42", plan: [], goalsList: [{ version: 1, text: "补齐客户端限流", created: now - 60_000 }], planRounds: [], timeline: [], artifacts: [], decisions: [], dependents: [], annotations: [] } })
    if (p === "/api/approval-policy/sessions/ses_1") return route.fulfill({ json: { session_id: "ses_1", status: "on", target: "auto", applied: "auto", desired: "auto", native_id: "bypassPermissions", refused: "sandbox policy forbids yolo", unknown: false, released: false } })
    if (p === "/api/sessions/ses_1/events") return route.fulfill({ json: { events: [], next: null } })
    if (p === "/api/sessions/ses_1/queued") return route.fulfill({ json: [] })
    if (p === "/api/sessions/ses_1/resume") return route.fulfill({ json: { harness: "claude-code", id: "sess_9f2a1b3c4d", cwd: "/tmp/acme-api", cwd_exists: true, command: "claude --resume sess_9f2a1b3c4d", transcript: null } })
    if (p === "/api/kb/uses") return route.fulfill({ json: { hits: [] } })
    if (p.startsWith("/api/bots/") && p.endsWith("/execution-nodes")) return route.fulfill({ json: { editable: true, bindings: [], candidates: [], unavailable_bindings: 0 } })
    if (p === "/api/stream") return route.abort("blockedbyclient")
    if (p.startsWith("/api/stats/")) return route.fulfill({ status: 404, json: {} })
    return route.fulfill({ status: 404, json: {} })
  })

  await page.addInitScript(() => { localStorage.setItem("steve-lang", "zh-CN"); localStorage.setItem("steve-onboarded", "1") })

  // 1) Local Node Harness tab: unified list (1 set, 0 RuntimeSetupPanel) + typed AddHarness (0 raw JSON <textarea>)
  await page.goto(base + "/#/devices/nodes/local/harness")
  await page.locator('[data-node-harnesses="local"]').waitFor()
  await page.locator('[data-node-harness-id="claude-code"]').waitFor()
  assert.equal(await page.locator('[data-node-harnesses="local"]').count(), 1)
  assert.equal(await page.locator("[data-panel=runtimes]").count(), 0, "no duplicate RuntimeSetupPanel")
  await page.locator("[data-add-harness]").click()
  const form = page.locator("[data-harness-form]")
  await form.waitFor()
  assert.equal(await form.locator("textarea").count(), 0, "AddHarness has zero raw JSON textareas")
  await form.locator("[data-harness-id]").fill("my-acp")
  await form.locator("[data-harness-name]").fill("My ACP")
  await form.locator("[data-harness-command]").fill("my-acp-cli")
  await form.locator("[data-harness-arg-input]").fill("--stdio")
  await form.locator("[data-harness-arg-add]").click()
  await form.locator('[data-harness-arg-item="--stdio"]').waitFor()
  await form.locator("[data-harness-nested-sandbox]").click()
  await form.locator("[data-harness-supports-resume]").click()
  await form.locator("[data-harness-resume-cli]").fill("my-acp --resume {id}")
  await form.locator("[data-harness-save]").click()
  await form.waitFor({ state: "detached" })
  assert.equal(addedHarnesses.length, 1)
  assert.deepEqual(addedHarnesses[0], {
    id: "my-acp",
    name: "My ACP",
    command: "my-acp-cli",
    args: ["--stdio"],
    properties: {
      modes: { ask: "default", plan: "plan", auto: "auto" },
      nested_sandbox: false,
      probe_initialize: true,
      resume_cli: "my-acp --resume {id}",
    },
  })

  // 2) Bot detail & Settings: no "Bot Bot", no zh-CN English leaks, 3 sub-tabs, sticky Save visible in first viewport
  await page.goto(base + "/#/bots/backend")
  await page.locator("h1").waitFor()
  assert.equal((await page.locator("h1").innerText()).trim(), "后端 Bot")
  assert.doesNotMatch(await page.locator("body").innerText(), /Bot\s+Bot/)
  await page.locator('[data-slot=tabs-trigger]:has-text("设置")').click()
  await page.locator("[data-bot-settings-tabs]").waitFor()
  const bodyText = await page.locator("body").innerText()
  assert.doesNotMatch(bodyText, /Agent Robot Avatar|Choose\.\.\.|Choose…/, "no English UI leak in zh-CN Bot Settings")
  const saveBox = await page.locator("[data-save-bot]").boundingBox()
  assert.ok(saveBox && saveBox.y >= 0 && saveBox.y + saveBox.height <= 860, "Save button is immediately visible without scrolling")
  for (const seg of ["identity", "model", "permissions"]) {
    await page.locator(`[data-bot-settings-tabs] [data-seg="${seg}"]`).click()
    await page.locator(`[data-bot-settings-tab="${seg}"]`).waitFor()
    const sections = await page.locator(`[data-bot-settings-tab="${seg}"] > section`).count()
    assert.ok(sections <= 4, `Bot Settings sub-tab ${seg} has ${sections} sections (max 4)`)
  }

  // 3) Item detail: single pending card, compact 2-line session card, details folded
  await page.goto(base + "/#/items/ST-42")
  await page.locator("[data-now]").waitFor()
  assert.equal(await page.locator('[data-decision-id="dec_pr_128"]').count(), 1)
  const nowText = await page.locator("[data-now]").innerText()
  assert.equal(nowText.split("合并 PR #128 (acme/acme-api) ？").length - 1, 1, "pending question appears once, not duplicated above card")
  const sessCard = page.locator('[data-item-session="ses_1"]')
  await sessCard.waitFor()
  assert.ok((await sessCard.boundingBox()).width >= 200, "session card spans sidebar width without narrow truncation")
  assert.match(await sessCard.innerText(), /后端 Bot/)
  assert.match(await sessCard.innerText(), /Claude Code · claude-opus-4-6/)
  assert.equal(await sessCard.locator("[data-item-session-details]").evaluate(el => el.open), false, "approval & nativeId folded by default")

  // 4) Session detail: Node printed once, NativeIdLine inside collapsible Runtime drawer, refusal reason inline
  await page.goto(base + "/#/sessions/ses_1")
  const runtimeBar = page.locator("[data-session-runtime]")
  await runtimeBar.waitFor()
  const rtText = await runtimeBar.innerText()
  assert.equal(rtText.split("Studio Mac mini").length - 1, 1, "Node name appears once in runtime strip")
  assert.equal(await runtimeBar.locator("[data-native-id]").count(), 0, "NativeIdLine moved out of main runtime strip")
  await runtimeBar.locator("[data-approval-refused]").waitFor()
  assert.match(await runtimeBar.locator("[data-approval-refused]").innerText(), /sandbox policy forbids yolo/, "refusal reason visible inline")
  await page.locator("[data-runtime-toggle]").click()
  await page.locator('[data-session-runtime-meta] [data-native-id="sess_9f2a1b3c4d"]').waitFor()

  assert.deepEqual(errors, [])
  console.log("local-harness-unify: ok")
} finally {
  await browser.close()
}
