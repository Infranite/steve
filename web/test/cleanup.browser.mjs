// Real cleanup/disconnect components with mocked Hub responses. No live API is reachable.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5196"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const results = []
try {
  for (const width of [390, 1280]) for (const mode of ["single", "changed", "multi"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 } })
    const errors = [], unexpected = [], mutations = []
    page.on("pageerror", e => errors.push(e.message))
    const plan = body => {
      const ids = body.level === "all" ? (body.connection_id === "beta" ? ["beta-decision"] : ["alpha-open", "alpha-resolved"]) : []
      return { name: "测试接入", level: body.level, spaces: 1, messages: 31, messages_purged: 2, contacts: 4, accounts: 4, decisions: ids.length, decision_ids: ids, kept: [], kept_count: 0 }
    }
    await page.route("**/*", route => {
      const request = route.request(), u = new URL(request.url())
      const reply = (data, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(data) })
      if (u.origin !== base) { unexpected.push(request.url()); return route.abort("blockedbyclient") }
      if (!u.pathname.startsWith("/api/")) return route.continue()
      if (u.pathname === "/api/channels/cleanup/preview") return reply(plan(request.postDataJSON()))
      if (/^\/api\/connections\/(alpha|beta)\/usage$/.test(u.pathname)) return reply({ flows: [], sources: [], spaces: 1 })
      if (u.pathname === "/api/channels/cleanup") {
        const body = request.postDataJSON(); mutations.push({ type: "cleanup", body })
        if (mode === "changed") return reply({ error: "关联决定已变化，请重新预览并确认清理" }, 409)
        return reply({ ...plan(body), backup: "fixture-backup.sqlite3" })
      }
      if (/^\/api\/connections\/(alpha|beta)$/.test(u.pathname) && request.method() === "DELETE") {
        mutations.push({ type: "disconnect", id: u.pathname.split("/").at(-1) }); return reply({})
      }
      unexpected.push(request.method() + " " + u.pathname); return route.abort("blockedbyclient")
    })
    await page.goto(base + "/test/fixtures/cleanup.html?mode=" + mode)
    await page.locator('[data-cleanup-choice="all"]').click()
    await page.locator("[data-cleanup-plan]").filter({ hasText: mode === "multi" ? "3 个关联决定" : "2 个关联决定" }).waitFor()
    assert.match(await page.locator("[data-cleanup-undo-note]").innerText(), /不能再撤销.*历史仍会保留/)
    assert.match(await page.locator("[data-cleanup-plan]").innerText(), /条保留消息的正文和附件/)
    const confirm = page.locator(mode === "multi" ? "[data-disconnect-confirm]" : "[data-cleanup-confirm]")
    assert.equal(await confirm.isDisabled(), true, "name confirmation remains required")
    await page.locator("[data-cleanup-typed]").fill("测试接入")
    await confirm.waitFor({ state: "visible" })
    assert.equal(await confirm.isEnabled(), true)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, "no horizontal overflow")
    if (shots) await page.screenshot({ path: `${shots}/cleanup-${mode}-${width}.png`, fullPage: true })
    await confirm.click()
    if (mode === "changed") {
      await page.getByText("关联决定已变化，请重新预览并确认清理", { exact: true }).waitFor()
      assert.equal(await page.locator("[data-fixture-state]").getAttribute("data-fixture-state"), "open")
      assert.equal(mutations.length, 1, "failure must not disconnect or retry automatically")
      assert.equal(await confirm.isEnabled(), true)
    } else {
      await page.locator('[data-fixture-state="done"]').waitFor()
      if (mode === "multi") assert.deepEqual(mutations.filter(m => m.type === "disconnect").map(m => m.id), ["beta", "alpha"])
    }
    const cleanups = mutations.filter(m => m.type === "cleanup")
    assert.deepEqual(cleanups.map(m => [m.body.connection_id, m.body.decision_ids]), mode === "multi"
      ? [["alpha", ["alpha-open", "alpha-resolved"]], ["beta", ["beta-decision"]]]
      : [["alpha", ["alpha-open", "alpha-resolved"]]])
    assert(cleanups.every(m => m.body.confirm === true && m.body.level === "all"))
    assert.deepEqual(unexpected, []); assert.deepEqual(errors, [])
    results.push({ width, mode, passed: true }); await page.close()
  }
  console.log(JSON.stringify({ passed: true, results, safety: "All API responses mocked; no real deletes" }))
} finally { await browser.close() }
