// Real SettingsView/full app, with API fixtures (not a real Hub/platform acceptance).
// Only /api/ and the full app's /auth/hello are intercepted; /src/api/ stays executable source.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5211", shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const defaultPolicy = { enabled: false, mode: "log", party_id: null, space_id: null, timezone: "UTC", quiet_start: null, quiet_end: null }
const me = { id: "self-party", name: "Morgan", is_me: true, status: "active", kind: "real", aliases: [], rels: ["me"], tags: [], org: null, avatar: null, note: "", visibility: "private", jobs: [], attrs: [], teams: [], merges: [], reports_to: [] }
const target = { party_id: me.id, space_id: "self-bot-dm", party_name: me.name, space_name: "Morgan · Helper bot", channel: "ext_ws" }
const entry = (id, extra = {}) => ({ id, event_id: "event-" + id, mode: "log", body: "Work item completed: Portable notification UI\nProject: Release checks", state: "held", delivery_id: null, seen_at: null, created: 1794646200000, ...extra })
const state = { version: "fixture", web_version: "fixture", owner: false, workspace: { id: "ws_1", name: "Steve", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [{ id: "ws_1", name: "Steve", description: "", item_prefix: "ST", sort: 0, created: 0 }], items: [], decisions: [], recent: [], bots: [], allBots: [], projects: [], tags: [], tagUses: {}, thread: [], settings: { theme: "light" }, sessions: [], devices: [], runtimes: [], notifications: [{ id: "workspace-notice", kind: "system", level: "info", text: "Workspace notice stays separate", item_id: null, created: 1794646200000 }], notifySeen: 0 }
const errors = []
async function open(options = {}) {
  const model = { policy: { ...defaultPolicy }, targets: [target], inbox: [entry("n1")], policyFail: false, putFail: false, targetsFail: false, meFail: false, inboxFail: false, seenFail: false, ...options }
  const width = options.width ?? 1440, theme = options.theme ?? "light", lang = options.lang ?? "zh-CN"
  const page = await browser.newPage({ locale: lang, viewport: { width, height: 1000 }, colorScheme: theme, reducedMotion: "reduce" })
  page.setDefaultTimeout(12000)
  page.on("pageerror", (e) => errors.push(e.message))
  await page.addInitScript(({ theme, lang }) => { localStorage.setItem("steve-lang", lang); localStorage.setItem("steve-onboarded", "1"); document.addEventListener("DOMContentLoaded", () => document.documentElement.classList.toggle("dark", theme === "dark")) }, { theme, lang })
  const calls = []
  const otherWorkspace = { ...state.workspace, id: "ws_2", name: "Second workspace" }
  let releaseState, signalState
  const stateGate = new Promise((resolve) => { releaseState = resolve })
  const stateRequested = new Promise((resolve) => { signalState = resolve })
  await page.route((u) => u.pathname === "/auth/hello" || u.pathname.startsWith("/api/"), async (route) => {
    const req = route.request(), p = new URL(req.url()).pathname, method = req.method()
    const workspace = req.headers()["x-steve-ws"]
    const resource = workspace === "ws_2" ? options.otherModel : model
    calls.push({ method, path: p, workspace, origin: new URL(req.url()).origin, body: req.postData() ? req.postDataJSON() : null })
    const json = (body, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })
    const error = () => json({ error: "fixture unavailable" }, 503)
    if (p === "/auth/hello") return json({ name: "steve", api: 1, version: "fixture", login: { token: true, oidc: false } })
    if (p === "/api/workspaces/ws_2/switch") return json({ ok: true })
    if (p === "/api/state") {
      if (options.otherModel && workspace === "ws_2") { signalState(); await stateGate }
      return json({ ...state, ...(options.otherModel ? { workspace: workspace === "ws_2" ? otherWorkspace : state.workspace, workspaces: [state.workspace, otherWorkspace] } : {}), settings: { theme } })
    }
    if (p === "/api/me") return json({ user: { id: "user-member", name: me.name, email: null, role: "member" }, version: "fixture", web_version: "fixture" })
    if (p === "/api/me/contact") return model.meFail ? error() : json(me)
    if (p === "/api/me/notification-targets") return model.targetsFail ? error() : json({ targets: model.targets })
    if (p === "/api/me/notification-policy" && method === "GET") return model.policyFail ? error() : json(resource.policy)
    if (p === "/api/me/notification-policy" && method === "PUT") {
      if (model.putFail) return error()
      resource.policy = req.postDataJSON(); return json({ ok: true })
    }
    if (p === "/api/me/notifications") return model.inboxFail ? error() : json(resource.inbox)
    if (p.endsWith("/seen") && p.startsWith("/api/me/notifications/")) {
      if (model.seenFail) return error()
      resource.inbox = resource.inbox.map((n) => n.id === p.split("/").at(-2) ? { ...n, seen_at: Date.now() } : n)
      return json({ ok: true })
    }
    if (p === "/api/plugins/themes") return json({ themes: [] })
    if (p === "/api/onboard") return json({ state: "done" })
    return json({ error: "fixture route absent" }, 404)
  })
  await page.goto(base + (options.fullApp ? "/#/settings/notify" : `/test/fixtures/settings-panes.html${theme === "dark" ? "?theme=dark" : ""}#/settings/notify`))
  await page.locator("[data-personal-notifications]").waitFor()
  return { page, calls, model, releaseState, stateRequested }
}
const choose = async (page, hook, name) => { await page.locator(hook).click(); await page.getByRole("option", { name, exact: false }).click() }
const fits = async (page) => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no overflow at ${(await page.viewportSize()).width}`)
const saved = (page) => page.locator("[data-notify-save-status]").filter({ hasText: "已保存" })
try {
  // GET error and retry; never render/save a fabricated default policy.
  {
    const { page, model, calls } = await open({ policyFail: true })
    await page.getByRole("alert").filter({ hasText: "未能加载你的通知策略" }).waitFor()
    assert.equal(await page.locator("[data-notify-enabled]").count(), 0)
    model.policyFail = false
    await page.getByRole("alert").filter({ hasText: "未能加载你的通知策略" }).getByRole("button", { name: "重试" }).click()
    await page.locator("[data-notify-enabled]").waitFor()
    assert.equal(await page.locator("[data-notify-enabled]").isChecked(), false)
    assert.equal(calls.filter((c) => c.method === "PUT").length, 0)
    await page.close()
  }
  // Real PUT shape, explicit consent, overnight minute inputs, failure is not reported as saved.
  {
    const { page, model, calls } = await open()
    await page.locator("[data-notify-enabled]").click()
    await page.locator("[data-notify-consent]").waitFor()
    await choose(page, "[data-notify-mode]", "即时 · Bot 私聊")
    assert.ok(await page.locator("[data-notify-save]").isDisabled(), "no automatic target")
    await choose(page, "[data-notify-target]", target.space_name)
    await page.locator("[data-notify-timezone]").fill("Asia/Shanghai")
    await page.locator("[data-notify-quiet]").click()
    await page.getByLabel("开始 · 当地时间", { exact: true }).fill("23:15"); await page.keyboard.press("Tab")
    await page.getByLabel("结束 · 当地时间", { exact: true }).fill("06:45"); await page.keyboard.press("Tab")
    model.putFail = true
    await page.locator("[data-notify-save]").click()
    await page.locator("[data-notify-save-error]").waitFor()
    assert.match(await saved(page).innerText(), /已关闭/)
    assert.equal(model.policy.enabled, false)
    assert.equal(await page.locator("[data-notify-enabled]").isChecked(), true, "failed save keeps edits")
    const payload = { enabled: true, mode: "now", party_id: me.id, space_id: target.space_id, timezone: "Asia/Shanghai", quiet_start: 1395, quiet_end: 405 }
    assert.deepEqual(calls.filter((c) => c.method === "PUT").at(-1).body, payload)
    model.putFail = false
    await page.locator("[data-notify-save]").click()
    await page.locator("[data-notify-save-status]").filter({ hasText: /^已保存$/ }).waitFor()
    assert.deepEqual(model.policy, payload)
    assert.equal(await page.locator("[data-notify-consent]").count(), 0)
    await page.getByLabel("结束 · 当地时间", { exact: true }).fill("23:15"); await page.keyboard.press("Tab")
    assert.ok(await page.locator("[data-notify-save]").isDisabled(), "equal boundaries invalid")
    await page.getByLabel("结束 · 当地时间", { exact: true }).fill("06:45"); await page.keyboard.press("Tab")
    await page.locator("[data-notify-timezone]").fill("Mars/Olympus")
    assert.ok(await page.locator("[data-notify-save]").isDisabled())
    await page.close()
  }
  // No bot targets means no immediate mode; failed discovery retries independently.
  {
    const { page, model } = await open({ targets: [], targetsFail: true, inbox: [] })
    const alert = page.getByRole("alert").filter({ hasText: "未能检查 Bot 会话" })
    await alert.waitFor(); model.targetsFail = false
    await alert.getByRole("button", { name: "重试" }).click()
    await page.locator("[data-notify-no-targets]").waitFor()
    await page.locator("[data-notify-mode]").click()
    assert.equal(await page.getByRole("option", { name: "即时 · Bot 私聊", exact: true }).getAttribute("aria-disabled"), "true")
    await page.keyboard.press("Escape")
    await page.getByText("还没有个人通知", { exact: true }).waitFor()
    await page.close()
  }
  // An unreachable saved target remains unavailable, never silently redirected to the other reachable DM.
  {
    const { page, model } = await open({ policy: { ...defaultPolicy, enabled: true, mode: "now", party_id: me.id, space_id: "gone-dm" } })
    await page.getByRole("alert").filter({ hasText: "已保存的会话暂不可达" }).waitFor()
    assert.ok(await page.locator("[data-notify-save]").isDisabled())
    await page.locator("[data-notify-enabled]").click(); await page.locator("[data-notify-save]").click()
    await page.locator("[data-notify-save-status]").filter({ hasText: /^已保存$/ }).waitFor()
    assert.equal(model.policy.space_id, "gone-dm"); assert.equal(model.policy.enabled, false)
    await page.close()
  }
  // Inbox retry, personal read failure/success, retention, and no workspace dismissal API call.
  {
    const { page, model, calls } = await open({ inboxFail: true, seenFail: true, inbox: [entry("n1"), entry("n2", { body: null, state: "purged", seen_at: 1 }), entry("n3", { state: "submitted", seen_at: 1 })] })
    const alert = page.getByRole("alert").filter({ hasText: "未能加载个人收件箱" })
    await alert.waitFor(); model.inboxFail = false
    await alert.getByRole("button", { name: "重试" }).click()
    const row = page.locator('[data-personal-notification="n1"]')
    await row.locator("[data-personal-seen]").click()
    await row.getByRole("alert").waitFor(); assert.match(await row.innerText(), /未读/)
    model.seenFail = false
    await row.getByRole("button", { name: "重试" }).click()
    await row.getByText(/仅积累 · 已读/).waitFor()
    assert.equal(await row.locator("[data-personal-seen]").count(), 0)
    assert.match(await page.locator('[data-personal-notification="n2"]').innerText(), /正文已按保留规则/)
    assert.match(await page.locator('[data-personal-notification="n3"]').innerText(), /送达未确认/)
    assert.deepEqual(calls.filter((c) => c.method === "POST").map((c) => c.path), ["/api/me/notifications/n1/seen", "/api/me/notifications/n1/seen"])
    await page.close()
  }
  // Empty provider labels stay localized, never display opaque IDs as a human-readable option.
  {
    const { page, model } = await open({ targets: [{ ...target, space_name: "", party_name: "" }] })
    await choose(page, "[data-notify-mode]", "即时 · Bot 私聊")
    await choose(page, "[data-notify-target]", "未命名会话")
    assert.match(await page.locator("[data-notify-target]").innerText(), /未命名会话/)
    await page.locator("[data-notify-enabled]").click(); await page.locator("[data-notify-save]").click()
    await page.locator("[data-notify-save-status]").filter({ hasText: /^已保存$/ }).waitFor()
    assert.equal(model.policy.space_id, target.space_id)
    await page.close()
  }
  // Identity failure is not replaced with the workspace owner; disable is still possible.
  {
    const { page, model } = await open({ meFail: true })
    await page.getByRole("alert").filter({ hasText: "未能加载本人关联身份" }).waitFor()
    await page.locator("[data-notify-enabled]").click()
    assert.ok(await page.locator("[data-notify-save]").isDisabled())
    model.meFail = false
    await page.getByRole("alert").filter({ hasText: "未能加载本人关联身份" }).getByRole("button", { name: "重试" }).click()
    await page.getByText(me.name, { exact: true }).waitFor()
    assert.ok(await page.locator("[data-notify-save]").isEnabled())
    await page.close()
  }
  // Full-app A→B switch: B state stalls while A's real form remains clickable.
  // Saving/reading that old form must remain in A, never cancel B's pending delivery.
  {
    const otherModel = { policy: { ...defaultPolicy, enabled: true, mode: "now", party_id: me.id, space_id: target.space_id }, inbox: [entry("b-waiting", { state: "waiting" })] }
    const unchangedB = structuredClone(otherModel)
    const { page, calls, model, stateRequested, releaseState } = await open({ fullApp: true, otherModel, policy: { ...defaultPolicy, enabled: true, party_id: me.id } })
    await page.locator("[data-notify-enabled]").waitFor()
    await page.locator("[data-ws-switch]").click()
    await page.locator('[data-ws-opt="Second workspace"]').click()
    await stateRequested
    assert.ok(await page.locator("[data-notify-enabled]").isChecked(), "old A form is still mounted in the delayed state window")
    await page.locator("[data-notify-enabled]").click()
    await page.locator("[data-notify-save]").click()
    await page.locator("[data-notify-save-status]").filter({ hasText: /^已保存$/ }).waitFor()
    await page.locator('[data-personal-notification="n1"] [data-personal-seen]').click()
    await page.locator('[data-personal-notification="n1"]').getByText(/仅积累 · 已读/).waitFor()
    assert.equal(model.policy.enabled, false)
    assert.notEqual(model.inbox[0].seen_at, null)
    const writes = calls.filter((c) => c.method === "PUT" || c.path.endsWith("/seen"))
    assert.equal(writes.length, 2)
    assert.ok(writes.every((c) => c.workspace === "ws_1"), "all old-form writes stay bound to A")
    assert.deepEqual(otherModel, unchangedB, "B policy and pending notification stay unchanged")
    releaseState()
    await page.waitForFunction(() => location.hash === "#/")
    await page.goto(base + "/#/settings/notify")
    await page.locator('[data-personal-notification="b-waiting"]').waitFor()
    assert.ok(await page.locator("[data-notify-enabled]").isChecked(), "B must not inherit A's disabled draft")
    assert.equal(await page.locator('[data-personal-notification="n1"]').count(), 0)
    assert.deepEqual(otherModel, unchangedB)
    await page.close()
  }
  // The actual full app, desktop/mobile viewports, both themes; accumulation modes explain their limits.
  for (const theme of ["light", "dark"]) for (const width of [1440, 390]) {
    const { page, calls } = await open({ theme, width, fullApp: true, policy: { ...defaultPolicy, quiet_start: 1320, quiet_end: 420 }, inbox: [entry("n1"), entry("n2", { body: "Long word: " + "portable".repeat(70), state: "submitted", seen_at: 1 })] })
    await page.locator("[data-notify-enabled]").waitFor()
    await choose(page, "[data-notify-mode]", "摘要 · 仅积累")
    assert.match(await page.locator("[data-notify-mode-scope]").innerText(), /只积累.*不生成.*不定时发送/)
    await fits(page)
    assert.equal(await page.evaluate(() => document.documentElement.classList.contains("dark")), theme === "dark")
    if (shots) {
      await page.screenshot({ path: `${shots}/notifications-${width}-${theme}.png`, fullPage: true })
      await page.locator("[data-personal-notification]").last().scrollIntoViewIfNeeded()
      await page.screenshot({ path: `${shots}/notifications-inbox-${width}-${theme}.png`, fullPage: true })
      await fits(page)
    }
    await choose(page, "[data-notify-mode]", "记录 · 仅收件箱")
    assert.match(await page.locator("[data-notify-mode-scope]").innerText(), /不发消息.*不定时发送/)
    await choose(page, "[data-notify-mode]", "即时 · Bot 私聊")
    await choose(page, "[data-notify-target]", target.space_name)
    await page.locator("[data-notify-enabled]").click()
    await page.locator("[data-notify-consent]").waitFor()
    await page.locator("[data-notify-mode]").scrollIntoViewIfNeeded()
    await fits(page)
    if (shots) await page.screenshot({ path: `${shots}/notifications-now-${width}-${theme}.png`, fullPage: true })
    const enabled = page.locator("[data-notify-enabled]")
    await enabled.hover()
    const hover = await enabled.evaluate((el) => {
      const wrapper = el.closest(".ui-swc"), track = el.nextElementSibling
      const probe = document.createElement("span"); probe.style.background = "var(--aui-primary-hover)"; wrapper.append(probe)
      const expected = getComputedStyle(probe).backgroundColor; probe.remove()
      return { actual: getComputedStyle(track).backgroundColor, expected }
    })
    assert.equal(hover.actual, hover.expected, "enabled hover uses this pane's primary hover token")
    await page.locator("[data-notify-mode]").focus(); await page.keyboard.press("Shift+Tab")
    assert.ok(await enabled.evaluate((el) => el.matches(":focus-visible") && getComputedStyle(el.nextElementSibling).outlineStyle !== "none"), "keyboard focus ring stays visible")
    if (shots) await page.screenshot({ path: `${shots}/notifications-enabled-focus-${width}-${theme}.png`, fullPage: true })
    await page.locator("[data-notify-quiet]").click()
    await enabled.hover()
    await fits(page)
    if (shots) await page.screenshot({ path: `${shots}/notifications-switch-states-${width}-${theme}.png`, fullPage: true })
    assert.ok(calls.some((c) => c.path === "/auth/hello"))
    assert.equal(calls.some((c) => c.path.startsWith("/src/")), false)
    await page.close()
  }
  {
    const { page } = await open({ lang: "en", inbox: [], targets: [] })
    await page.getByText("My notification inbox", { exact: true }).waitFor()
    assert.match(await page.locator("[data-notify-mode-scope]").innerText(), /No messages or scheduled summaries/)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("personal notifications browser fixtures: ok (including full app, 1440/390, light/dark)")
} finally { await browser.close() }
