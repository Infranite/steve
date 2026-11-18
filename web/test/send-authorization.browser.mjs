import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
/* Mock API contract regression, NOT actual authorization or external-send acceptance.
 * Exercises state -> waitOf -> decision-card -> real ProposalCard on desktop and phone. */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5196", shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 844 } }), errors = [], mutations = []
    page.on("pageerror", e => errors.push(e.message))
    let enabled = false, available = true, readFailed = false, conflict = false, lost = false, kept = false, hold = null, release = null, decision = "dec_f", count = 0
    await page.route("**/*", async route => {
      const u = new URL(route.request().url()), path = decodeURIComponent(u.pathname)
      if (u.origin !== new URL(base).origin) return route.abort()
      if (path === "/api/chan-messages/cm_out/context") return route.fulfill({ json: { space: { id: "sp1", name: "合成权限回归群", kind: "group", external: false }, as_you: true, persona: null, text: await page.evaluate(() => window.__body).catch(() => null), reply_to: null, before: [] } })
      if (path === "/api/conversations/chan:sp1/record") return route.fulfill({ json: { records: [{ id: "cm_out", status: "failed", text: await page.evaluate(() => window.__body).catch(() => null), out: { status: "failed", decision, error: "没有开启 msg.send_as_user" } }], has_more: false, next: null } })
      if (path === "/api/connections/conn_review") return route.fulfill(readFailed ? { status: 503, json: { error: "连接回读失败" } } : { json: { id: "conn_review", caps: enabled ? ["msg.send_as_user"] : [], available_caps: available ? ["msg.send_as_user"] : [], status: "ok" } })
      if (path === "/api/chan-messages/cm_out/authorization") {
        mutations.push({ action: "create" })
        if (hold === "create") await new Promise(r => { release = r })
        return route.fulfill({ json: { id: "proposal_" + ++count, title: "允许这个连接以你本人发送（不会发送任何消息）", target: { kind: "channel_send_permission", id: "conn_review" }, href: "#/channels/acc/conn_review", changes: [{ label: "本地代发能力 msg.send_as_user（整个连接）", from: "关闭", to: "开启；平台授权及每条消息的审批仍分别校验；可撤销" }] } })
      }
      if (path.startsWith("/api/config/")) {
        const [, , , id, action] = path.split("/")
        mutations.push({ id, action })
        if (hold === action) await new Promise(r => { release = r })
        if (conflict) return route.fulfill({ status: 409, json: { error: action === "undo" ? "这次改动已经不能撤销了" : "这份草稿已经失效，请重新说一次" } })
        if (action === "apply") enabled = true
        if (action === "undo" && !kept) enabled = false
        if (lost) return route.abort()
        return route.fulfill({ json: { ok: true, ...(action === "undo" && kept ? { kept: ["连接已再次修改，保留当前代发能力"] } : {}) } })
      }
      if (path.startsWith("/api/")) return route.fulfill({ status: 503, json: { error: "mock fixture: no such route" } })
      return route.continue()
    })
    const card = page.locator("[data-decision-id]").first()
    const calls = () => page.evaluate(() => JSON.parse(JSON.stringify(window.__calls)))
    const open = async () => {
      enabled = false; available = true; readFailed = false; conflict = false; lost = false; kept = false; hold = null; decision = "dec_f"; count = 0; mutations.length = 0
      await page.goto(base + "/test/fixtures/handoff-card.html?managed")
      await page.evaluate(async () => { sessionStorage.clear(); await window.__refresh() })
      // The fixture starts without Wait.recovery; only a state refresh can pass it through waitOf.
      await page.locator('[data-authorization-current="disabled"]').waitFor()
    }
    const click = async selector => { await card.locator(selector).click(); await page.waitForFunction(() => !document.querySelector('[data-send-submitting]')) }
    const noOverflow = async () => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no overflow at ${width}`)

    await open()
    assert.ok((await card.locator('[data-send-source]').innerText()).includes("steve.connection_capability"))
    for (const value of ["missing_permission", "connection", "managed", "fixture.user", "capability_disabled", "conn_review", "sp1"]) assert.ok((await card.locator('[data-send-recovery-info]').innerText()).includes(value))
    assert.equal(await card.locator('[data-send-settings]').getAttribute("href"), "#/channels/acc/conn_review")
    assert.equal(await card.locator('[data-send-body]').textContent(), await page.evaluate(() => window.__body))

    hold = "create"
    await card.locator('[data-authorization-create]').dblclick({ force: true })
    await page.waitForFunction(() => document.querySelector('[data-send-submitting]'))
    await card.locator('[data-fail="retry"]').click({ force: true })
    assert.deepEqual(mutations, [{ action: "create" }], "create is guarded synchronously")
    assert.deepEqual(await calls(), [], "no send while creating a proposal")
    hold = null; release()
    await card.locator('[data-proposal="draft"]').waitFor()
    assert.ok((await card.locator('[data-change]').innerText()).includes("整个连接"))
    assert.ok((await card.locator('[data-change]').innerText()).includes("关闭"))
    assert.ok((await card.locator('[data-change]').innerText()).includes("开启"))
    assert.ok((await card.locator('[data-authorization-target]').innerText()).includes("conn_review"))
    await noOverflow()
    if (shots) await page.screenshot({ path: `${shots}/managed-review-${width}.png`, fullPage: true })
    hold = "apply"
    await card.locator('[data-apply]').dblclick({ force: true })
    await page.waitForFunction(() => document.querySelector('[data-send-submitting]'))
    assert.equal(await card.locator('[data-apply]').isDisabled(), true)
    assert.equal(await card.locator('[data-fail="rewrite"]').isDisabled(), true)
    await card.locator('[data-fail="discard"]').click({ force: true })
    assert.equal(mutations.filter(x => x.action === "apply").length, 1)
    assert.deepEqual(await calls(), [], "applying permission is never retry")
    hold = null; release()
    await page.locator('[data-authorization-current="enabled"]').waitFor()
    await card.locator('[data-authorization-result="applied"]').waitFor()
    assert.equal(await card.locator('[data-send-status]').getAttribute("data-send-status"), "failed")
    // Reload restores only the last proposal receipt; current capabilities still come from GET.
    await page.reload(); await page.evaluate(() => window.__refresh())
    await card.locator('[data-authorization-current="enabled"]').waitFor()
    await card.locator('[data-undo]').waitFor()
    if (shots) await page.screenshot({ path: `${shots}/managed-applied-${width}.png`, fullPage: true })
    hold = "undo"
    await card.locator('[data-undo]').dblclick({ force: true })
    await page.waitForFunction(() => document.querySelector('[data-send-submitting]'))
    await card.locator('[data-fail="retry"]').click({ force: true })
    assert.equal(mutations.filter(x => x.action === "undo").length, 1, "undo is guarded while busy")
    assert.deepEqual(await calls(), [], "undo cannot be combined with a send")
    hold = null; release()
    await card.locator('[data-authorization-current="disabled"]').waitFor()
    await card.locator('[data-authorization-result="undone"]').waitFor()
    assert.deepEqual(await calls(), [])
    assert.equal(await card.locator('[data-apply]').count(), 0, "an undone changeset cannot be applied again")
    // A retry while access is off creates a new failure decision, rather than reviving the resolved one.
    await page.evaluate(() => { window.__stateStale = true })
    await card.locator('[data-fail="retry"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.equal((await calls()).length, 1)
    decision = "dec_f_again"
    await page.evaluate(async () => { window.__decisionId = "dec_f_again"; await window.__refresh() })
    await page.locator('[data-decision-id="dec_f_again"]').waitFor()
    await click('[data-authorization-create]')
    await click('[data-apply]')
    await card.locator('[data-authorization-current="enabled"]').waitFor()
    assert.equal((await calls()).length, 1)
    await card.locator('[data-fail="retry"]').click()
    await page.waitForFunction(() => window.__calls.length === 2)
    assert.deepEqual(await calls(), [{ id: "dec_f", choice: "retry" }, { id: "dec_f_again", choice: "retry" }])

    // Stale proposal: show the exact 409, read back current state, require fresh review.
    await open(); await click('[data-authorization-create]')
    conflict = true; await click('[data-apply]')
    await card.locator('[data-authorization-result="stale"]').waitFor()
    assert.ok((await card.locator('[data-authorization-error]').innerText()).includes("已经失效"))
    assert.equal(await card.locator('[data-apply]').count(), 0)
    assert.deepEqual(await calls(), [])
    conflict = false; await click('[data-authorization-create]'); await click('[data-apply]')
    conflict = true; await click('[data-undo]')
    assert.ok((await card.locator('[data-authorization-error]').innerText()).includes("不能撤销"))
    await card.locator('[data-authorization-current="enabled"]').waitFor()

    await open(); await click('[data-authorization-create]'); await click('[data-apply]')
    kept = true; await click('[data-undo]')
    assert.ok((await card.locator('[data-authorization-kept]').innerText()).includes("保留当前代发能力"))
    await card.locator('[data-authorization-current="enabled"]').waitFor()
    assert.equal(await card.locator('[data-authorization-create]').isDisabled(), true)

    // A successful mutation with failed readback is explicit and cannot silently repeat apply.
    await open(); await click('[data-authorization-create]'); readFailed = true; await click('[data-apply]')
    await card.locator('[data-authorization-read-error]').waitFor()
    assert.equal(await card.locator('[data-undo]').isDisabled(), true)
    readFailed = false; await click('[data-send-refresh]')
    await card.locator('[data-authorization-current="enabled"]').waitFor()
    await page.waitForFunction(() => document.querySelector('[data-undo]')?.disabled === false)
    assert.equal(await card.locator('[data-undo]').isDisabled(), false)

    await open(); await click('[data-authorization-create]'); lost = true; await click('[data-apply]')
    await card.locator('[data-authorization-result="uncertain"]').waitFor()
    await card.locator('[data-authorization-current="enabled"]').waitFor()
    assert.equal(await card.locator('[data-apply]').count(), 0, "lost mutation response must not offer blind reapply")
    lost = false; await click('[data-authorization-undo-unknown]')
    await card.locator('[data-authorization-current="disabled"]').waitFor()
    await card.locator('[data-authorization-result="undone"]').waitFor()

    // External metadata preserves verified error scope/codes and links, with no managed authorization button.
    await open()
    await page.evaluate(async () => {
      window.__recovery = { kind: "channel_refusal", source: "ext_ws", scope: "target", code: "target_forbidden", rpc_code: -32016, authorization_kind: "external", authorization_url: "/api/chan-messages/cm_out/authorization", connection_id: "conn_review", settings_href: "#/channels/acc/conn_review", instructions: "到服务管理端检查目标发送范围，必要时联系管理员。", external_policy_known: false }
      await window.__refresh()
    })
    await page.waitForFunction(() => !document.querySelector('[data-send-authorization]'))
    for (const value of ["ext_ws", "target", "target_forbidden", "-32016", "external"]) assert.ok((await card.innerText()).includes(value))
    assert.equal(await card.locator('[data-authorization-create]').count(), 0)
    assert.equal(await card.locator('[data-apply]').count(), 0)
    assert.equal(mutations.length, 0)
    assert.ok((await card.innerText()).includes("不会授予外部权限"))
    await noOverflow()
    if (shots) await page.screenshot({ path: `${shots}/external-recovery-${width}.png`, fullPage: true })
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("send-authorization ok — mocked API; 1280/390 managed review/apply/undo/re-fail/review/apply/manual retry, double click, reload, 409, kept, readback, external boundary")
} finally { await browser.close() }
