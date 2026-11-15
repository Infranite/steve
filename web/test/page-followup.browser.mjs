/* Real page components against a fixture-only API. No request may reach a Hub or mutate external state. */
import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5213"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })
const locale = process.env.STEVE_TEST_LOCALE || "en"
const connection = { id: "conn_mail", account_id: "acc_mail", adapter: "imap", adapter_name: "Mailbox", identity: "self", name: "Work mailbox", handle: "mail@example.com", status: "ok", health: { last_ok: 1, last_error: "", error_at: 0 }, last_sync: 1, capabilities: [] }
const integrations = { areas: [], callback_urls: [], providers: [{ id: "mail", name: "Mail", status: "supported", areas: [], adapters: [], connections: [connection] }] }
const conversation = (space, text) => ({ conversation: { space, space_id: space.replace("chan:", ""), name: "Release coordination", provider: "Chat", dm: false, kind: "group", system: "chat", member_count: 5, mode: "bot", has_external: false }, messages: [{ id: "fixture-message", role: "assistant", author: "steve", text, created: Date.now(), space }] })
let runs = 0
try {
  for (const width of [1440, 390]) for (const scheme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width, height: 900 }, colorScheme: scheme, locale, reducedMotion: "reduce" })
    page.setDefaultTimeout(12000)
    const errors = [], writes = []
    page.on("pageerror", e => errors.push(e.message))
    let mode = "error", ownerMode = "error", peopleOn = true, pending = [], requests = 0
    await page.route("**/*", async route => {
      const req = route.request(), u = new URL(req.url())
      if (u.origin !== base) return route.abort()
      if (!u.pathname.startsWith("/api/")) return route.continue()
      if (req.method() !== "GET") { writes.push(`${req.method()} ${u.pathname}`); return route.fulfill({ status: 405, json: { error: "fixture forbids writes" } }) }
      if (u.pathname.includes("/conversations/") && u.pathname.endsWith("/messages")) {
        requests++
        const space = decodeURIComponent(u.pathname.split("/")[3]), current = mode
        if (current === "delay") { pending.push(route); return }
        if (current === "network") return route.abort("failed")
        if (current === "error") return route.fulfill({ status: 500, json: { error: "Temporary fixture failure" } })
        if (current === "missing") return route.fulfill({ status: 404, json: { error: "not_found" } })
        return route.fulfill({ json: conversation(space, current) })
      }
      if (u.pathname === "/api/me/contact") return route.fulfill(peopleOn ? { json: { id: "me", name: "You", is_me: true, avatar: null } } : { status: 404, json: { error: "not_found" } })
      if (u.pathname === "/api/connections/integrations") return route.fulfill({ json: integrations })
      if (u.pathname === "/api/stewards/connection/conn_mail") {
        if (ownerMode === "network") return route.abort("failed")
        if (ownerMode === "missing") return route.fulfill({ status: 404, json: { error: "not_found" } })
        if (ownerMode === "error") return route.fulfill({ status: 500, json: { error: "Temporary owner failure" } })
        return route.fulfill({ json: { object: { object_type: "connection", object_id: "conn_mail", type_label: "Connection", name: "Work mailbox" }, stewards: [{ role: "owner", status: "confirmed", person_id: "alice", person_name: "Alice", is_me: false }] } })
      }
      return route.fulfill({ status: 404, json: { error: "fixture" } })
    })
    const shot = async name => { if (shots) await page.screenshot({ path: `${shots}/${name}-${width}-${scheme}.png`, fullPage: true }) }
    const fits = async () => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `overflow: ${width}/${scheme}`)
    const refresh = () => page.evaluate(() => window.dispatchEvent(new Event("fixture-refresh")))
    const waitPending = async () => {
      const until = Date.now() + 12000
      while (!pending.length && Date.now() < until) await page.waitForTimeout(20)
      assert.ok(pending.length, "Expected a delayed fixture request")
    }
    try {
      await page.goto(base + "/test/fixtures/chat-layout.html?conv=chan:release")
      {
        await page.locator("[data-conv-error]").waitFor()
        assert.equal(await page.locator("[data-conv-missing]").count(), 0)
        await fits(); await shot("conversation-error")
        mode = "Loaded conversation message"
        await page.locator("[data-conv-retry]").click()
        await page.getByText(mode, { exact: true }).waitFor()
        mode = "network"; await refresh()
        await page.locator("[data-conv-error]").waitFor()
        await page.getByText("Loaded conversation message", { exact: true }).waitFor()
        assert.equal(await page.locator("[data-conv-missing]").count(), 0)
        await fits(); await shot("conversation-retained")
        mode = "Recovered conversation message"; await page.locator("[data-conv-retry]").click()
        await page.getByText(mode, { exact: true }).waitFor()
        assert.equal(await page.locator("[data-conv-error]").count(), 0)
        mode = "missing"; await refresh(); await page.locator("[data-conv-missing]").waitFor()
        assert.equal(await page.locator("[data-conv-error]").count(), 0)
        await shot("conversation-missing")
        mode = "delay"; await refresh()
        await page.waitForFunction(() => document.querySelector("[data-conv-loading]"))
        await waitPending()
        mode = "New workspace message"
        await page.evaluate(() => window.dispatchEvent(new CustomEvent("fixture-scope", { detail: "ws-new" })))
        await page.getByText(mode, { exact: true }).waitFor()
        for (const r of pending.splice(0)) await r.fulfill({ status: 500, json: { error: "Old workspace failure" } })
        await page.waitForTimeout(100)
        assert.equal(await page.locator("[data-conv-error]").count(), 0)
        await page.getByText(mode, { exact: true }).waitFor()
        mode = "delay"; await refresh()
        await waitPending()
        mode = "New conversation message"
        await page.evaluate(() => window.dispatchEvent(new CustomEvent("fixture-conversation", { detail: "chan:another" })))
        await page.getByText(mode, { exact: true }).waitFor()
        for (const r of pending.splice(0)) await r.fulfill({ json: conversation("chan:release", "Stale conversation message") })
        await page.waitForTimeout(100)
        assert.equal(await page.getByText("Stale conversation message", { exact: true }).count(), 0)
        mode = "delay"; await refresh(); await waitPending()
        mode = "New Hub message"
        await page.evaluate(() => {
          // Same fixture origin, a different Hub scope; no real Hub is contacted.
          localStorage.setItem("steve.hub", location.origin)
          window.dispatchEvent(new CustomEvent("fixture-scope", { detail: "ws-new" }))
        })
        await page.getByText(mode, { exact: true }).waitFor()
        for (const r of pending.splice(0)) await r.fulfill({ status: 500, json: { error: "Old Hub failure" } })
        await page.waitForTimeout(100)
        assert.equal(await page.locator("[data-conv-error]").count(), 0)
        await page.getByText(mode, { exact: true }).waitFor()
        assert.ok(requests >= 10)
      }
      await page.goto(base + "/test/fixtures/comm-pages.html?view=provider" + (scheme === "dark" ? "&dark" : ""))
      await page.locator("[data-conn-owner]").waitFor()
      {
        await page.locator("[data-conn-owner] [role=alert]").waitFor()
        await fits(); await shot("owner-error")
        ownerMode = "ok"
        await page.locator("[data-conn-owner]").getByRole("button", { name: /^(Retry|重试)$/ }).click()
        await page.locator("[data-conn-owner] [data-owner-name]").getByText("Alice", { exact: true }).waitFor()
        assert.equal(await page.locator("[data-conn-owner] [role=alert]").count(), 0)
        ownerMode = "missing"; await page.reload()
        await page.locator("[data-conn-owner] [role=alert]").waitFor()
        assert.match(await page.locator("[data-conn-owner]").innerText(), /Couldn't load the owner|负责人载入失败/)
        ownerMode = "network"; await page.reload()
        await page.locator("[data-conn-owner] [role=alert]").waitFor()
        assert.match(await page.locator("[data-conn-owner]").innerText(), /Couldn't load the owner|负责人载入失败/)
        peopleOn = false; await page.reload()
        await page.locator("[data-conn-owner] [role=status]").waitFor()
        assert.match(await page.locator("[data-conn-owner]").innerText(), /Owner information is unavailable|负责人信息暂不可用/)
        assert.equal(await page.locator("[data-conn-owner] button").count(), 0)
        await fits()
      }
      assert.deepEqual(writes, []); assert.deepEqual(errors, [])
      runs++
    } catch (e) { console.error(await page.locator("[data-chat-messages]").innerText().catch(() => page.locator("body").innerText()), errors); await shot("failure"); throw e } finally { await page.close() }
  }
  console.log(JSON.stringify({ passed: true, locale, runs, widths: [1440, 390], themes: ["light", "dark"], source: "fixture-only; no real Hub" }))
} finally { await browser.close() }
