// The comm pages lead with what needs you, against a fake Hub: 群聊与私聊 puts a broken account and an unset new
// group in one notice and says per row what Steve does there; a group's page says how many replies wait for you,
// shows the latest five messages and folds the rarely changed settings; an account's trigger words are one switch
// until they are on; 连接 with a handful of services shows them without search or categories. 1280 and 390 px.
import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5199"
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

const now = Date.now(), m = 60_000
const health = (err = "") => ({ last_ok: now - m, last_error: err, error_at: err ? now : 0 })
const conn = (id, account_id, adapter, status, err) => ({ id, account_id, adapter, auth: "credential", status, caps: ["msg.receive"], available_caps: [], health: health(err), last_sync: now })
const connections = [conn("conn_bot", "acc_bot", "feishu_app", "ok"), conn("conn_me", "acc_me", "feishu_user", "ok"), conn("conn_ext", "acc_ext", "ext_ws", "error", "bridge did not answer")]
const account = (id, name) => ({ id, system: { id: "feishu", name: "飞书", kind: "im" }, party: { id: "p" + id, name, type: "bot", is_me: false }, kind: "app_bot", handle: id, display_name: name, verified: true, confidence: 1, source: "", source_label: "", connection: null, vault_items: [], revision: 1 })
const accounts = { acc_bot: account("acc_bot", "值班机器人"), acc_me: account("acc_me", "我"), acc_ext: account("acc_ext", "夜班桥") }
const persona = { id: "per_1", name: "小运", avatar: {}, bot_id: "steve", intro: "", soul: "", reveal: { name: true, role: true, ai: true, steve: false, owner: true }, signature: "", is_default: true, intro_preview: "", used_by: { accounts: [{ account_id: "acc_bot", name: "值班机器人" }], spaces: [] }, revision: 1 }
const space = (o) => ({ connection_id: "conn_bot", kind: "group", member_count: 9, has_external: false, status: "active", persona: { id: "per_1", name: "小运", inherited: true }, duty: "", rules: null, effective_rules: [{ when: { kind: "mention_or_dm" }, act: "reply" }], flow_id: null, muted: false, collab: false,
  memory_scope: "space", disclosure: "conclusion", item_defaults: { tags: [], project_id: null, bot_id: null }, configured: true, teams: [], revision: 1, last_message: now - m, attention: "realtime", role: "answer", identities: [], conflicts: [], ...o })
const spaces = [space({ id: "sp_ops", name: "值班群" }), space({ id: "sp_new", name: "周会", configured: false }), space({ id: "sp_off", name: "告警推送", attention: "blocked" }),
  space({ id: "sp_ext", name: "夜班", connection_id: "conn_ext", persona: null, role: "draft", voice: { connection_id: "conn_ext", mode: "self", name: "夜班桥", account_id: "acc_ext", chosen: false } })]
const msg = (n, o = {}) => ({ id: "cm" + n, space_id: "sp_ops", direction: "in", sender: { name: "同事" + n, party_id: null }, persona_id: null, on_behalf: "none", text: "第 " + n + " 条", status: "received", work_item_id: null, decision_id: null, created: now - n * m, sent: null, purged: null, basis: null, ...o })
const recent = [msg(1), msg(2, { direction: "out", sender: { name: "值班机器人" }, status: "draft", decision_id: "dc_1" }), msg(3), msg(4), msg(5), msg(6)]
const providers = [
  { id: "feishu", name: "飞书 / Lark", status: "supported", areas: [{ area: "channel", state: "supported" }], adapters: [{ id: "feishu_app", name: "机器人", identity: "bot", auth: "credential", device: false, capabilities: [{ area: "channel", caps: ["msg.receive"], scopes: [], planned: false }] }, { id: "feishu_user", name: "你本人", identity: "self", auth: "oauth", device: true, capabilities: [{ area: "channel", caps: ["msg.receive"], scopes: [], planned: false }] }],
    connections: [{ id: "conn_bot", account_id: "acc_bot", adapter: "feishu_app", adapter_name: "机器人", identity: "bot", name: "值班机器人", handle: "cli_x", status: "ok", health: health(), last_sync: now, capabilities: [{ area: "channel", state: "missing", missing_scopes: ["im:chat:readonly"], suggested_scopes: [], caps: [] }] }] },
  { id: "ext_ws", name: "其他聊天工具", status: "supported", areas: [{ area: "channel", state: "supported" }], adapters: [{ id: "ext_ws", name: "其他聊天工具", identity: "self", auth: "credential", device: false, capabilities: [{ area: "channel", caps: ["msg.receive"], scopes: [], planned: false }] }], connections: [] },
]
const integrations = { areas: [{ id: "channel", label: "聊天" }], providers, callback_urls: [] }

const run = async ({ width, scheme }) => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 }, colorScheme: scheme, reducedMotion: "reduce" })
  page.setDefaultTimeout(10000)
  const errors = [], writes = []; page.on("pageerror", (e) => errors.push(e.message))
  await page.route("**/*", async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith("/api/")) return route.continue()
    const method = route.request().method(), p = u.pathname.slice(4)
    if (method !== "GET") { writes.push(method + " " + p); return route.fulfill({ status: 200, contentType: "application/json", body: "{}" }) }
    let data
    if (p === "/connections") data = connections
    else if (p.startsWith("/accounts/")) data = accounts[p.split("/")[2]]
    else if (p === "/spaces") data = spaces
    else if (p === "/spaces/sp_ops") data = { ...spaces[0], recent }
    else if (p === "/personas") data = [persona]
    else if (p === "/connections/integrations") data = integrations
    else if (p === "/channels/conn_me") data = { connection_id: "conn_me", persona: null, rules: [{ when: { kind: "dm" }, act: "draft" }], trigger: { on: false, words: ["/bot"], where: ["*"], mark: true }, send_delay_s: null, send_delay_default_s: 3 }
    else if (p.startsWith("/channels/")) data = []
    else return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "fixture" }) })
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(data) })
  })
  const q = scheme === "dark" ? "&dark" : ""
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/${name}-${width}-${scheme}.png`, fullPage: true }) }
  try {
    // 群聊与私聊: what needs you in one notice; each row says what Steve does there; blocked groups wait behind their filter
    await page.goto(base + "/test/fixtures/comm-pages.html?view=list" + q); await page.locator("[data-channels-attention]").waitFor()
    assert.equal(await page.locator('[data-attention-account="conn_ext"]').count(), 1)
    assert.match(await page.locator("[data-attention-new]").innerText(), /周会/)
    assert.equal(await page.locator('[data-space-row="告警推送"]').count(), 0)
    assert.equal(await page.locator('[data-space-item="sp_ops"] [data-space-doing]').innerText(), "小运 直接回复 · 值班机器人")
    assert.equal(await page.locator('[data-space-item="sp_ext"] [data-space-doing]').innerText(), "替你起草，等你批准 · 夜班桥")
    await fits(); await shot("chats")
    // a group: the reply waiting for you, the latest five messages, the settings you rarely change folded
    await page.goto(base + "/test/fixtures/comm-pages.html?view=space" + q); await page.locator("[data-space-waiting]").waitFor()
    assert.match(await page.locator("[data-space-waiting]").innerText(), /1 条回复等你批准/)
    assert.equal(await page.locator("[data-recent] [data-msg]").count(), 5)
    await page.locator("[data-all-msgs]").click()
    assert.equal(await page.locator("[data-recent] [data-msg]").count(), 6)
    assert.equal(await page.locator('[data-select="space-disclosure"]').count(), 0)
    await page.locator("[data-space-more] button").first().click()
    await page.locator('[data-select="space-disclosure"]').waitFor()
    await fits(); await shot("space")
    // your own account: trigger words off are one switch
    await page.goto(base + "/test/fixtures/comm-pages.html?view=account" + q); await page.locator("[data-trigger-on]").waitFor()
    assert.equal(await page.locator("[data-trigger-scope]").count(), 0)
    await fits(); await shot("account")
    // 连接 with two services: no search or categories, and the one with missing permissions in the notice
    await page.goto(base + "/test/fixtures/comm-pages.html?view=integrations" + q); await page.locator("[data-integrations-attention]").waitFor()
    assert.equal(await page.locator("[data-adc-search]").count(), 0)
    assert.equal(await page.locator('[data-provider-row="feishu"]').count(), 1)
    await fits(); await shot("integrations")
    assert.deepEqual(writes, [])
    assert.deepEqual(errors, [])
  } finally { await page.close() }
}
try {
  for (const v of [{ width: 1280, scheme: "light" }, { width: 390, scheme: "dark" }]) await run(v)
  console.log("comm-pages ok")
} finally { await browser.close() }
