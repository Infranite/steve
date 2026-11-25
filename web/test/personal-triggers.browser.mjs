// Vite-only fixture over mocked Hub APIs: TriggerSection ignore-prefix add/reload/remove and
// RecordRow skipped/drafted handling display at 390 and 1280 px.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const web = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const now = Date.now()
try {
  for (const width of [390, 1280]) {
    let trigger = { on: true, words: ["/bot"], where: ["*"], who: ["me"], reply: "thread", mark: true, ignore_prefixes: [] }
    const records = [
      {
        id: `m-skip-${width}`, space_id: "sp_1", direction: "in",
        sender: { name: "我", party_id: "p_me" }, persona_id: null, on_behalf: "none",
        text: "/bot synthetic skip", status: "received", work_item_id: null, decision_id: null,
        created: now - 20_000, sent: null, purged: null, basis: null, turn: null,
        handling: { state: "skipped", label: "", reason: "No explicit request", error: null, item: null, decision: null },
      },
      {
        id: `m-draft-${width}`, space_id: "sp_1", direction: "in",
        sender: { name: "我", party_id: "p_me" }, persona_id: null, on_behalf: "none",
        text: "/bot draft synthetic answer", status: "received", work_item_id: null, decision_id: "dec_draft",
        created: now - 10_000, sent: null, purged: null, basis: null, turn: null,
        handling: { state: "drafted", label: "", reason: "", error: null, item: null, decision: "dec_draft" },
      },
    ]
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 900 } })
    const errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", async (route) => {
      const req = route.request(), u = new URL(req.url())
      if (u.origin !== new URL(web).origin) return route.abort("blockedbyclient")
      if (!u.pathname.startsWith("/api/")) return route.continue()
      const m = req.method(), p = decodeURIComponent(u.pathname)
      if (p === "/api/channels/conn_me" && m === "GET") return route.fulfill({ json: { connection_id: "conn_me", mode: "self", trigger } })
      if (p === "/api/channels/conn_me" && m === "PATCH") {
        const body = req.postDataJSON()
        if (body?.trigger) trigger = { ...trigger, ...body.trigger }
        return route.fulfill({ json: { connection_id: "conn_me", mode: "self", trigger } })
      }
      if (p === "/api/spaces" && m === "GET") return route.fulfill({ json: [] })
      if (p === "/api/conversations/chan:sp_1/record" && m === "GET") return route.fulfill({ json: { records, has_more: false, next: null } })
      return route.fulfill({ status: 404, json: { error: "fixture" } })
    })
    await page.goto(`${web}/test/fixtures/personal-triggers.html?account=conn_me`)
    await page.locator("[data-trigger-prefix]").fill("BROWSER_AUTO:")
    await page.locator("[data-trigger-prefix-add]").click()
    await page.getByRole("button", { name: "移除前缀 BROWSER_AUTO:" }).waitFor()
    await page.reload()
    await page.getByRole("button", { name: "移除前缀 BROWSER_AUTO:" }).waitFor()
    assert.deepEqual(trigger.ignore_prefixes, ["BROWSER_AUTO:"])

    await page.goto(`${web}/test/fixtures/personal-triggers.html?space=sp_1`)
    const skippedRow = page.locator(`[data-msg="m-skip-${width}"] [data-handling="skipped"]`)
    await skippedRow.waitFor()
    assert.match(await skippedRow.innerText(), /已跳过.*No explicit request/s)
    await page.locator(`[data-msg="m-draft-${width}"] [data-handling="drafted"]`).waitFor()
    assert.ok(await page.locator(`[data-msg="m-draft-${width}"]`).innerText().then((s) => s.includes("仅起草，未发送")))
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (process.env.STEVE_TEST_OUTPUT) await page.screenshot({ path: `${process.env.STEVE_TEST_OUTPUT}/draft-${width}.png`, fullPage: true })

    await page.goto(`${web}/test/fixtures/personal-triggers.html?account=conn_me`)
    await page.getByRole("button", { name: "移除前缀 BROWSER_AUTO:" }).click()
    await page.getByRole("button", { name: "移除前缀 BROWSER_AUTO:" }).waitFor({ state: "detached" })
    assert.deepEqual(trigger.ignore_prefixes, [])
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("passed: mocked Hub API prefix add/reload/remove, skip reason and draft-only display at 390/1280px")
} finally { await browser.close() }
