// Browser smoke test: unified person identity & ActorAvatar across sidebar account,
// Contacts "Me", item timeline (matching user avatar, duplicate-name collaborators,
// bot robot avatar, system badge, hover tooltips), settled decisions, and account login settings.
// All fixtures strictly match web/src/gen types (MemberIdentity, ItemView, DecisionView).
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"

const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({
  headless: true,
  ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}),
})
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5195"
const shots = process.env.STEVE_AVATAR_SHOTS || process.env.STEVE_TEST_SHOTS || ""
if (shots) await mkdir(shots, { recursive: true })

/* Fixed midday UTC timestamp paired with timezoneId: "UTC" + page.clock.setFixedTime(NOW)
 * so all fixture offsets (-120..0 min) stay on the same calendar day regardless of wall clock. */
const NOW = Date.UTC(2026, 10, 18, 14, 0)
const MIN = 60_000

/** @type {import("../src/gen/MemberIdentity").MemberIdentity} */
const ME_BASE = {
  user_id: "usr_owner",
  party_id: "ct_lin_01",
  name: "林知行",
  login: "lin.zx",
  email: "lin@example.test",
  role: "owner",
  subtitle: "负责人 · 星河科技",
}

/** @type {import("../src/gen/MemberIdentity").MemberIdentity} */
const ZHOU_FE = {
  user_id: "usr_zhou_fe",
  party_id: "ct_zhou_alpha",
  name: "周明",
  login: "zhou.fe",
  email: "zhou.fe@example.test",
  role: "member",
  subtitle: "前端 · 星河",
}

/** @type {import("../src/gen/MemberIdentity").MemberIdentity} */
const ZHOU_BE = {
  user_id: "usr_zhou_be",
  party_id: "ct_zhou_omega",
  name: "周明",
  login: "zhou.be",
  email: "zhou.be@example.test",
  role: "member",
  subtitle: "后端 · 星河",
}

const BOTS = [
  { id: "steve", name: "Steve", role: "总管", main: true, harness: "claude-code", model: null, reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "default", avatar: { color: "#8b7cf6" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: NOW, workspaces: ["default"], memories: 0 },
  { id: "code", name: "开发", role: "实现与评审", main: false, harness: "claude-code", model: null, reasoning_effort: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode: "default", avatar: { color: "#2b8a57" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: NOW, workspaces: ["default"], memories: 0 },
]

/** @type {import("../src/gen/ItemView").ItemView} */
const ITEM_VIEW = {
  id: "ST-42",
  ws: "default",
  no: 42,
  title: "统一全站人物头像与操作者身份",
  state: { kind: "active" },
  cycle: 1,
  revision: 3,
  delivery_revision: 1,
  delivery: [],
  bot_id: "code",
  priority: "高",
  star: true,
  source: "chat",
  created: NOW - 120 * MIN,
  updated: NOW - 2 * MIN,
  goals: 1,
  tags: [],
  detail_stamp: 1,
}

/** @type {import("../src/gen/DecisionView").DecisionView[]} */
const SETTLED_DECISIONS = [
  {
    id: "dec_pr_merge",
    ws: "default",
    kind: "approve",
    subject_type: "gh_pr_merge",
    subject_id: "ST-42",
    item_id: "ST-42",
    session_id: null,
    question: { kind: "raw", text: "合并 PR #526 到 main 分支？" },
    draft: "",
    options: ["yes", "no"],
    status: "resolved",
    result: { approved: true, choice: "合并 PR #526" },
    requested_by: "code",
    resolved_by: "usr_owner",
    created: NOW - 25 * MIN,
    resolved: NOW - 20 * MIN,
    undo_until: null,
    space_id: null,
    tier: "decide",
    folded: null,
    revision: 1,
  },
  {
    id: "dec_fe_review",
    ws: "default",
    kind: "approve",
    subject_type: "permission",
    subject_id: "ST-42",
    item_id: "ST-42",
    session_id: null,
    question: { kind: "raw", text: "允许更新设计令牌并重新截图？" },
    draft: "",
    options: ["yes", "no"],
    status: "resolved",
    result: { approved: true, choice: "允许本次" },
    requested_by: "code",
    resolved_by: "usr_zhou_fe",
    created: NOW - 40 * MIN,
    resolved: NOW - 35 * MIN,
    undo_until: null,
    space_id: null,
    tier: "decide",
    folded: null,
    revision: 1,
  },
  {
    id: "dec_yolo",
    ws: "default",
    kind: "approve",
    subject_type: "permission",
    subject_id: "ST-42",
    item_id: "ST-42",
    session_id: null,
    question: { kind: "raw", text: "运行静态类型检查与单测？" },
    draft: "",
    options: ["yes", "no"],
    status: "resolved",
    result: { approved: true, choice: "自动放行", decided_by: "yolo", reason: "只读验证命令" },
    requested_by: "code",
    resolved_by: "yolo",
    created: NOW - 55 * MIN,
    resolved: NOW - 50 * MIN,
    undo_until: null,
    space_id: null,
    tier: "decide",
    folded: null,
    revision: 1,
  },
  {
    id: "dec_expired",
    ws: "default",
    kind: "ask",
    subject_type: "item",
    subject_id: "ST-42",
    item_id: "ST-42",
    session_id: null,
    question: { kind: "raw", text: "是否跳过旧版迁移测试？" },
    draft: "",
    options: ["yes", "no"],
    status: "expired",
    result: { choice: "已过期" },
    requested_by: "code",
    created: NOW - 80 * MIN,
    resolved: NOW - 70 * MIN,
    undo_until: null,
    space_id: null,
    tier: "decide",
    folded: null,
    revision: 1,
  },
]

const ITEM_DETAIL = {
  id: "ST-42",
  goalsList: [{ version: 1, text: "让头像与显示名归属于联系人（人），时间线、决定、对话、成员列表全站统一使用 ActorAvatar。", created: NOW - 120 * MIN }],
  plan: [],
  planRounds: [],
  artifacts: [],
  annotations: [],
  decisions: SETTLED_DECISIONS,
  dependents: [],
  source: ["对话", "chat"],
  timeline: [
    { id: 1, ts: NOW - 60 * MIN, actor: "system", kind: "sys", text: "建立事项 ST-42", session_id: null },
    { id: 2, ts: NOW - 50 * MIN, actor: "usr_zhou_fe", kind: "me", text: "前端亮/暗主题与圆角比例已核对，视觉一致。", session_id: null },
    { id: 3, ts: NOW - 40 * MIN, actor: "code", kind: "sys", text: "提交了 PR #526（统一 ActorAvatar 与人物身份）", session_id: null },
    { id: 4, ts: NOW - 30 * MIN, actor: "usr_zhou_be", kind: "me", text: "迁移 125 与 decisions.resolved_by 已验证通过。", session_id: null },
    { id: 5, ts: NOW - 20 * MIN, actor: "usr_owner", kind: "me", text: "批准合并 PR #526", session_id: null },
    { id: 6, ts: NOW - 10 * MIN, actor: "system", kind: "sys", text: "CI 检查全部通过", session_id: null },
  ],
}

async function runTheme(theme) {
  const page = await browser.newPage({
    locale: "zh-CN",
    timezoneId: "UTC",
    viewport: { width: 1280, height: 860 },
    colorScheme: theme,
    reducedMotion: "reduce",
  })
  page.setDefaultTimeout(12000)
  await page.clock.setFixedTime(NOW)
  const errors = []
  page.on("pageerror", (e) => errors.push(e.message))

  let me = { ...ME_BASE }
  let myContact = {
    id: me.party_id,
    name: me.name,
    is_me: true,
    kind: "real",
    aliases: [],
    rels: ["me"],
    tags: [],
    org: { id: "org_1", name: "星河科技", dept: null, title: "负责人" },
    avatar: null,
    note: "",
    status: "active",
    visibility: "private",
    profile: { checked: NOW, next_check: null, source: null, fields: {} },
    jobs: [{ org_id: "org_1", org_name: "星河科技", dept: null, title: "负责人", from: null, to: null, source: "manual" }],
    attrs: [
      { id: "a1", key: "call", label: "称呼", value: "林知行", source: "me", source_label: "你确认", status: "confirmed", level: "public" },
      { id: "a2", key: "email", label: "邮箱", value: "lin@example.test", source: "profile", source_label: "Hub 用户资料", status: "confirmed", level: "public" },
    ],
    teams: [],
    merges: [],
    reports_to: [],
  }
  let avatarPng = null

  const shellState = () => ({
    version: "fixture",
    web_version: "fixture",
    workspace: { id: "default", name: "星河研发", description: "", item_prefix: "ST", sort: 0, created: NOW },
    workspaces: [{ id: "default", name: "星河研发", description: "", item_prefix: "ST", sort: 0, created: NOW }],
    me,
    members: [me, ZHOU_FE, ZHOU_BE],
    bots: BOTS,
    allBots: BOTS.map((b) => ({ id: b.id, name: b.name, workspaces: b.workspaces })),
    projects: [],
    tags: [],
    tagUses: {},
    settings: {},
    devices: [],
    runtimes: [],
  })

  await page.addInitScript((th) => {
    localStorage.setItem("steve-lang", "zh-CN")
    localStorage.setItem("steve-theme", th)
  }, theme)

  await page.route("**/*", async (route) => {
    const u = new URL(route.request().url())
    if (u.origin !== base) return route.abort()
    const p = u.pathname
    const method = route.request().method()
    const json = (body, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })

    if (p === "/auth/hello") return json({ name: "steve", version: "fixture", api: 2, login: { token: false, oidc: false } })
    if (!p.startsWith("/api/")) return route.continue()

    if (p === "/api/plugins/themes") return json({ themes: [] })
    if (p === "/api/shell") return json(shellState())
    if (p === "/api/me") return json({ user: { id: me.user_id, name: me.name, login: me.login, email: me.email, role: me.role, party_id: me.party_id, avatar: me.avatar, subtitle: me.subtitle }, person: me, auth: "cookie", token: false })
    if (p === "/api/summary") return json({ ws: "default", workspace_name: "星河研发", thinking: false, hold: null })
    if (p === "/api/yolo") return json({ on: false, active: false, forced_off: null, forced_reason: null, paused: null, window: null, scopes: [], counts: { decided: 1, declined: 0, scheduled: 0 } })
    if (p === "/api/snapshot") return json({ epoch: "fixture", rev: 1, cursor: "fixture:1", views: { item: [ITEM_VIEW], session: [], decision: [] } })
    if (p === "/api/views") return json({ rev: 1, views: [], missing: [] })
    if (p === "/api/views/page") {
      return json({ views: u.searchParams.get("kind") === "decision" ? SETTLED_DECISIONS : [], next: null, rev: 1 })
    }
    if (p === "/api/items/ST-42") return json(ITEM_DETAIL)
    if (p === "/api/items/ST-42/workflow") return json(null)
    if (p === "/api/items/ST-42/dependency-wait") return json(null)
    if (p.startsWith("/api/stats/items/")) return json({
      usage: { total: 12400, est: 0.08, reported: 0, hit_rate: 0.62, coverage: { state: "complete", total: 12400, covered: 12400 } },
      facts: { agent_ms: 180_000, wait_ms: 60_000, done_at: null },
      age_ms: 7_200_000,
    })
    if (p === "/api/items/ST-42/people") return json([
      { party_id: me.party_id, name: me.name, type: "contact", role: "owner", confidence: 1, avatar: me.avatar },
      { party_id: ZHOU_FE.party_id, name: ZHOU_FE.name, type: "contact", role: "participant", confidence: 1, avatar: null },
      { party_id: ZHOU_BE.party_id, name: ZHOU_BE.name, type: "contact", role: "participant", confidence: 1, avatar: null },
    ])
    if (p === "/api/me/contact" || p === "/api/contacts/me" || p === `/api/contacts/${me.party_id}`) return json(myContact)
    if (p === `/api/contacts/${me.party_id}/avatar` && method === "PUT") {
      avatarPng = route.request().postDataBuffer()
      me = { ...me, avatar: { file: "avatar_lin" } }
      myContact = { ...myContact, avatar: { file: "avatar_lin" } }
      return json(myContact)
    }
    if (p === "/api/files/avatar_lin" && avatarPng) {
      return route.fulfill({ status: 200, contentType: "image/png", body: avatarPng })
    }
    if (p.endsWith("/holdings")) return json({ accounts: [], responsible: [] })
    if (p === "/api/access") return json({
      me: me.user_id,
      all: true,
      current: "cli_now",
      users: { [me.user_id]: me.name, [ZHOU_FE.user_id]: ZHOU_FE.name },
      clients: [
        { id: "cli_now", user_id: me.user_id, name: "Chrome on macOS", kind: "browser", platform: "macOS", route: "local", created: NOW - 86_400_000, last_seen: NOW, revoked: null },
        { id: "cli_other", user_id: ZHOU_FE.user_id, name: "Safari on macOS", kind: "browser", platform: "macOS", route: "remote", created: NOW - 3_600_000, last_seen: NOW - 60_000, revoked: null },
      ],
      tokens: [],
    })
    if (p === "/api/devices" || p === "/api/nodes" || p === "/api/runtimes" || p === "/api/personas" || p === "/api/spaces" || p === "/api/accounts") return json([])
    if (p.startsWith("/api/parties/") || p.startsWith("/api/contacts/")) return json([])
    if (p === "/api/events") return route.abort("blockedbyclient")
    return json({}, 404)
  })

  // 1. Open item detail page and verify sidebar account + timeline actor consistency
  await page.goto(`${base}/#/items/ST-42`)
  await page.locator('[data-worklog]').waitFor()
  await page.locator('[data-account-avatar]').waitFor()
  // Let static robot snapshot settle before checking & screenshotting
  await page.waitForTimeout(350)

  const sidebar = page.locator('[data-sidebar="footer"]')
  const sidebarText = await sidebar.innerText()
  assert.ok(sidebarText.includes("林知行"), `sidebar shows person display name, got: ${sidebarText}`)
  assert.ok(!sidebarText.includes("lin.zx"), `sidebar must NOT show login handle, got: ${sidebarText}`)

  const sidebarAvatar = sidebar.locator('[data-account-avatar]')
  assert.equal(await sidebarAvatar.getAttribute("data-actor-id"), "ct_lin_01")
  assert.equal(await sidebarAvatar.getAttribute("data-actor-kind"), "person")

  const approvalRow = page.locator('[data-comment]').filter({ hasText: "批准合并 PR #526" })
  await approvalRow.waitFor()
  const approvalAvatar = approvalRow.locator('[data-actor-avatar]')
  assert.equal(await approvalAvatar.getAttribute("data-actor-id"), "ct_lin_01", "timeline approval actor matches sidebar account party_id")
  assert.equal(await approvalRow.locator('[data-timeline-actor]').innerText(), "林知行")

  // Initial + hue + background color match identically between sidebar and timeline approval
  const avatarSig = (loc) => loc.evaluate((el) => {
    const av = el.querySelector(".ui-avtc")
    const face = el.querySelector(".ui-avtc__face")
    return {
      text: av?.textContent?.trim() ?? null,
      hue: av?.getAttribute("data-hue") ?? null,
      bg: face ? getComputedStyle(face).backgroundColor : null,
    }
  })
  const sideSig = await avatarSig(sidebarAvatar)
  assert.ok(sideSig.hue && sideSig.bg, `sidebar avatar has hue and bg: ${JSON.stringify(sideSig)}`)
  assert.deepEqual(await avatarSig(approvalAvatar), sideSig, "sidebar and timeline show identical avatar for the same person")

  // Duplicate-named collaborators are disambiguated by both color and suffix
  const feRow = page.locator('[data-comment]').filter({ hasText: "前端亮/暗主题" })
  const beRow = page.locator('[data-comment]').filter({ hasText: "迁移 125" })
  assert.equal(await feRow.locator('[data-timeline-actor]').innerText(), "周明（前端 · 星河）")
  assert.equal(await beRow.locator('[data-timeline-actor]').innerText(), "周明（后端 · 星河）")
  const feSig = await avatarSig(feRow.locator('[data-actor-avatar]'))
  const beSig = await avatarSig(beRow.locator('[data-actor-avatar]'))
  assert.ok(feSig.hue && beSig.hue, `collaborators have hues: ${JSON.stringify([feSig, beSig])}`)
  assert.notEqual(feSig.hue, beSig.hue, `same-named people get distinct avatar hues (${feSig.hue} vs ${beSig.hue})`)
  assert.notEqual(feSig.bg, beSig.bg, `same-named people get distinct avatar colors (${feSig.bg} vs ${beSig.bg})`)

  // Bot & System timeline rows
  const botRow = page.locator('[data-timeline-sys]').filter({ hasText: "开发 Bot" })
  assert.equal(await botRow.locator('[data-actor-avatar]').getAttribute("data-actor-kind"), "bot")
  assert.equal(await botRow.locator('[data-actor-avatar]').getAttribute("data-actor-tooltip"), "开发 Bot · Bot")
  const sysRow = page.locator('[data-timeline-sys]').filter({ hasText: "CI 检查全部通过" })
  assert.equal(await sysRow.locator('[data-actor-avatar]').getAttribute("data-actor-kind"), "system")
  assert.equal(await sysRow.locator('[data-actor-avatar]').getAttribute("data-actor-tooltip"), "系统")

  // Hovering an actor avatar reveals tooltip with name + actor type
  await approvalAvatar.hover()
  await page.locator('[role="tooltip"]').filter({ hasText: "林知行 · 人" }).waitFor()
  await page.mouse.move(10, 10)
  await page.locator('[role="tooltip"]').waitFor({ state: "detached" })

  if (shots) await page.screenshot({ path: `${shots}/item-timeline-${theme}.png` })

  // 2. Contacts "Me" page: same avatar & display name as sidebar; uploading photo updates both!
  await page.goto(`${base}/#/contacts/me`)
  await page.locator('[data-avatar-edit]').waitFor()
  const mePageAvatar = page.locator('[data-avatar-edit] [data-actor-avatar]')
  assert.equal(await mePageAvatar.getAttribute("data-actor-id"), "ct_lin_01")
  assert.deepEqual(await avatarSig(mePageAvatar), await avatarSig(sidebarAvatar), "Contacts Me page matches sidebar avatar")

  if (shots) await page.screenshot({ path: `${shots}/contacts-me-${theme}.png` })

  // Upload a crisp photo and verify sidebar & item timeline immediately reflect the uploaded picture
  const png = Buffer.from(
    (await page.evaluate(() => {
      const c = document.createElement("canvas")
      c.width = c.height = 64
      const g = c.getContext("2d")
      g.fillStyle = "#5b4fe9"
      g.fillRect(0, 0, 64, 64)
      g.fillStyle = "#ffffff"
      g.beginPath()
      g.arc(32, 24, 12, 0, Math.PI * 2)
      g.fill()
      g.beginPath()
      g.arc(32, 56, 20, Math.PI, 0)
      g.fill()
      return c.toDataURL("image/png")
    })).split(",")[1],
    "base64",
  )
  await page.locator('[data-avatar-input]').setInputFiles({ name: "lin.png", mimeType: "image/png", buffer: png })
  await page.locator('[data-avatar-edit] img').waitFor()
  await sidebarAvatar.locator("img").waitFor()
  const uploadedSrc = await sidebarAvatar.locator("img").getAttribute("src")
  assert.equal(await page.locator('[data-avatar-edit] img').getAttribute("src"), uploadedSrc, "uploaded photo matches between Me page and sidebar")

  await page.goto(`${base}/#/items/ST-42`)
  const approvalImg = page.locator('[data-comment]').filter({ hasText: "批准合并 PR #526" }).locator('[data-actor-avatar] img')
  await approvalImg.waitFor()
  assert.equal(await approvalImg.getAttribute("src"), uploadedSrc, "uploaded photo propagates to item timeline actor")

  // 3. Inbox settled decisions show settler avatars (me, collaborator, Steve/YOLO, system)
  await page.goto(`${base}/#/inbox`)
  await page.locator("button", { hasText: "已处理" }).click()
  const doneRow = page.locator('[data-done-row="ST-42"]')
  await doneRow.waitFor()
  await doneRow.locator("button").first().click()
  await page.locator('[data-done-row="ST-42"] [data-actor-id="ct_lin_01"] img').first().waitFor()
  await page.locator('[data-done-row="ST-42"] [data-actor-id="ct_zhou_alpha"]').waitFor()
  await page.locator('[data-done-row="ST-42"] [data-actor-kind="bot"]').waitFor()
  await page.locator('[data-done-row="ST-42"] [data-actor-kind="system"]').waitFor()
  assert.match(await doneRow.innerText(), /周明（前端 · 星河）\s*选了/, "settled decision by collaborator shows disambiguated display name")

  if (shots) await page.screenshot({ path: `${shots}/inbox-done-${theme}.png` })

  // 4. Account login handle appears only in account/client settings (#/devices/clients)
  await page.goto(`${base}/#/devices/clients`)
  await page.locator('[data-client-current="cli_now"]').waitFor()
  assert.match(await page.locator('[data-client-current="cli_now"]').innerText(), /登录账号\s+lin\.zx/)
  assert.equal(await page.locator('[data-client="cli_other"] [data-actor-id="ct_zhou_alpha"]').count(), 1)

  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `no horizontal overflow in ${theme}`)
  assert.deepEqual(errors, [])
  await page.close()
}

try {
  await runTheme("light")
  await runTheme("dark")
  console.log(JSON.stringify({
    passed: true,
    scenarios: [
      "sidebar account uses person display name & avatar (never login handle)",
      "item timeline PR approval matches sidebar avatar identically",
      "duplicate-named collaborators get distinct colors & disambiguator suffixes",
      "bot robot avatar & system badge with hover tooltips",
      "photo upload propagates across Contacts Me, sidebar, timeline, and settled decisions",
      "login handle shown only in account/client settings",
      "light & dark themes",
    ],
  }))
} finally {
  await browser.close()
}
