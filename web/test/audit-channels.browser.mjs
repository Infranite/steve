import assert from "node:assert/strict"
/* The 2026-11-06 audit's channel cards: a flagged draft offers “改写后再看” (a plain one does not), an
 * unchecked mail draft offers “检查披露” and shows the result, and a bot account sets its undo window.
 * Mocked API, no Hub. */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
const draft = (verdict) => ({ id: "md_1", connection_id: "conn_mail", thread_id: "th_1", reply_to: null, mode: "reply", to: [{ addr: "alice@acme.example", name: "Alice", party_id: null, external: true }], cc: [], bcc: [], subject: "Re: 预算", body: "预算审批分三步。", files: [], author: "steve", item_id: null, session_id: null, decision_id: "dc_mail", basis: { verdict, entries: [], spans: [] }, status: "pending", send_at: null, error: null, sent_message: null, created: 1, updated: 1, sent: null })
const conn = { id: "conn_bot", account_id: "acc_bot", adapter: "feishu_app", auth: "credential", status: "ok", caps: ["msg.receive", "msg.send"], available_caps: [], health: { last_ok: 1, last_error: "", error_at: 0 }, last_sync: null }
const account = { id: "acc_bot", system: { id: "feishu", name: "飞书", kind: "im" }, party: { id: "p", name: "值班小运", type: "bot", is_me: false }, kind: "app_bot", handle: "cli_x", display_name: "值班小运", verified: true, confidence: 1, source: "", source_label: "", connection: null, vault_items: [], revision: 1 }
let assertions = 0
try {
  for (const [width, theme] of [[1280, "dark"], [390, "light"]]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 } }), errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    const decided = [], checked = [], patched = []
    let verdict = "unchecked", delay = null
    await page.route("**/*", async (route) => {
      const u = new URL(route.request().url()); if (u.origin !== base) return route.abort()
      if (!u.pathname.startsWith("/api/")) return route.continue()
      const m = route.request().method(), p = u.pathname
      if (p.startsWith("/api/decisions/") && m === "POST") { decided.push([p.split("/")[3], route.request().postDataJSON().choice]); return route.fulfill({ json: { id: p.split("/")[3], status: "resolved", result: {} } }) }
      if (p === "/api/mail/drafts/md_1" && m === "GET") return route.fulfill({ json: draft(verdict) })
      if (p === "/api/mail/drafts/md_1/check" && m === "POST") { checked.push(1); verdict = "pass"; return route.fulfill({ json: draft(verdict) }) }
      if (p === "/api/connections" && m === "GET") return route.fulfill({ json: [conn] })
      if (p === "/api/accounts/acc_bot") return route.fulfill({ json: account })
      if (p === "/api/personas") return route.fulfill({ json: [] })
      if (p === "/api/spaces") return route.fulfill({ json: [] })
      if (p === "/api/channels/conn_bot" && m === "GET") return route.fulfill({ json: { connection_id: "conn_bot", persona: null, rules: [], trigger: null, send_delay_s: delay, send_delay_default_s: 3 } })
      if (p === "/api/channels/conn_bot" && m === "PATCH") { const b = route.request().postDataJSON(); patched.push(b); delay = b.send_delay_s; return route.fulfill({ json: { connection_id: "conn_bot", persona: null, rules: [], trigger: null, send_delay_s: delay, send_delay_default_s: 3 } }) }
      return route.fulfill({ status: 404, json: { error: "fixture" } })
    })
    const q = theme === "dark" ? "&dark" : ""
    await page.goto(base + "/test/fixtures/audit-channels.html?cards" + q)
    // a flagged draft: the rewrite is offered; a plain one has no such button
    const rewrite = page.locator("[data-rewrite]")
    await rewrite.waitFor()
    assert.equal(await rewrite.count(), 1); assertions++
    assert.equal((await rewrite.textContent()).trim(), "改写后再看"); assertions++
    await rewrite.click()
    await page.waitForFunction(() => true)
    for (let n = 0; n < 50 && !decided.length; n++) await page.waitForTimeout(20)
    assert.deepEqual(decided[0], ["dc_chan", "rewrite"]); assertions++
    // the unchecked mail draft: “检查披露”, then the result
    const check = page.locator("[data-draft-check]")
    await check.waitFor()
    assert.equal((await check.textContent()).trim(), "检查披露"); assertions++
    await page.locator("[data-basis='unchecked']").getByText("没有做披露检查").waitFor()
    await check.click()
    await page.locator("[data-basis='pass']").getByText("披露检查通过").waitFor()
    assert.equal(checked.length, 1); assertions++
    assert.equal(await page.locator("[data-draft-check]").count(), 0); assertions++
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)); assertions++

    // the account's undo window
    await page.goto(base + "/test/fixtures/audit-channels.html?account" + q)
    const select = page.locator("[data-select='acc-send-delay']")
    await select.waitFor()
    await page.getByText("批准后等多久再发出").waitFor()
    assert.match(await select.textContent(), /默认（3 秒）/); assertions++
    await select.click()
    await page.getByRole("option", { name: "20 秒" }).click()
    for (let n = 0; n < 50 && !patched.length; n++) await page.waitForTimeout(20)
    assert.deepEqual(patched, [{ send_delay_s: 20 }]); assertions++
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)); assertions++
    assert.deepEqual(errors, []); assertions++
    await page.close()
  }
} finally {
  await browser.close()
}
console.log(`audit-channels: ${assertions} assertions passed`)
