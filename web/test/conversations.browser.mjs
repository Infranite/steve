import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5193"
let assertions = 0
try {
  for (const width of [390, 1280]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 844 } }), errors = []
    page.on("pageerror", e => errors.push(e.message))
    const rows = Array.from({ length: 237 }, (_, i) => ({ space: `chan:sp${i}`, space_id: `sp${i}`, name: `项目群 ${i}`, provider: "飞书", dm: false, member_count: 5, last_text: `最近消息 ${i}`, count: 1, last: 237-i, closed: false, sent: i % 3 === 0, pendingApproval: i % 3 === 1 }))
    let fail = false, releaseMutation, delayed = false
    await page.route("**/api/**", async route => {
      const u = new URL(route.request().url())
      if (!u.pathname.startsWith("/api/")) return route.continue()

      if (u.pathname === "/api/conversations") {
        const q = u.searchParams.get("q") || "", state = u.searchParams.get("state"), offset = Number(u.searchParams.get("offset") || 0)
        const filtered = rows.filter(r => (!q || r.name.includes(q)) && (state === "replied" ? r.sent || r.pendingApproval : state === "all" || (state === "closed" ? r.closed : !r.closed)))
        return route.fulfill({ json: { conversations: filtered.slice(offset, offset+50), has_more: offset+50<filtered.length, next_offset: offset+50, total: filtered.length } })
      }
      /* the list's account filter (#243): this workspace has no channel account to pick */
      if (u.pathname === "/api/conversations/channels") return route.fulfill({ json: { channels: [] } })
      if (u.pathname.endsWith("/preferences")) {
        if (delayed) await new Promise(resolve => { releaseMutation = resolve })
        if (fail) return route.fulfill({ status: 500, json: { error: "保存失败，请重试" } })
        const id = decodeURIComponent(u.pathname.split("/")[3]), row = rows.find(r => r.space === id)
        assert.ok(row); row.closed = route.request().postDataJSON().closed
        return route.fulfill({ json: { ok: true } })
      }
      throw new Error(`unexpected API ${u.pathname}`)
    })
    await page.goto(base + "/test/fixtures/conversations.html")
    assert.equal(await page.locator("[data-conversation-row]").count(), 0)
    await page.getByRole("button", { name: "打开群聊和私聊", exact: true }).click()
    await page.getByRole("button", { name: "打开：项目群 0", exact: true }).waitFor()
    assert.equal(await page.locator("[data-conversation-row]").count(), 50)
    await page.getByRole("button", { name: "加载更多" }).click()
    await page.getByRole("button", { name: "打开：项目群 99", exact: true }).waitFor()
    assert.equal(await page.locator("[data-conversation-row]").count(), 100)
    // Changing the filter resets pagination; sent and approval-only rows both qualify.
    const repliedResponse = page.waitForResponse(r => new URL(r.url()).pathname === "/api/conversations" && new URL(r.url()).searchParams.get("state") === "replied")
    await page.getByRole("radio", { name: "最近回复过的", exact: true }).click()
    await repliedResponse
    await page.waitForFunction(() => document.querySelectorAll("[data-conversation-row]").length === 50)
    await page.getByRole("button", { name: "打开：项目群 73", exact: true }).waitFor()
    assert.equal(await page.locator("[data-conversation-row]").count(), 50)
    assert.equal(await page.getByRole("radio", { name: "最近回复过的", exact: true }).getAttribute("aria-checked"), "true")
    assert.equal(await page.locator('[data-conversation-row="chan:sp0"]').count(), 1)
    assert.equal(await page.locator('[data-conversation-row="chan:sp1"]').count(), 1)
    assert.equal(await page.locator('[data-conversation-row="chan:sp2"]').count(), 0)
    await page.getByRole("button", { name: "加载更多" }).click()
    await page.getByRole("button", { name: "打开：项目群 148", exact: true }).waitFor()
    assert.equal(await page.locator("[data-conversation-row]").count(), 100)
    assert.ok((await page.locator("[data-conversation-row]").evaluateAll(els => els.map(el => el.dataset.conversationRow))).every(id => Number(id.split("sp")[1]) % 3 !== 2))
    await page.getByRole("textbox", { name: "搜索会话" }).fill("项目群 236")
    await page.getByText("没有匹配的会话", { exact: true }).waitFor()
    assert.equal(await page.locator("[data-conversation-row]").count(), 0)
    await page.getByRole("radio", { name: "未关闭", exact: true }).click()
    await page.getByRole("button", { name: "打开：项目群 236", exact: true }).waitFor()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.getByRole("textbox", { name: "搜索会话" }).fill("项目群 236")
    await page.getByRole("button", { name: "打开：项目群 236", exact: true }).waitFor()
    assert.equal(await page.locator("[data-conversation-row]").count(), 1)
    fail = true
    await page.getByRole("button", { name: "关闭：项目群 236", exact: true }).click()
    await page.getByRole("alert").waitFor()
    assert.equal(rows[236].closed, false)
    fail = false
    await page.getByRole("button", { name: "关闭：项目群 236", exact: true }).click()
    await page.getByText("没有匹配的会话", { exact: true }).waitFor()
    assert.equal(rows[236].closed, true)
    await page.reload()
    await page.getByRole("button", { name: "打开群聊和私聊", exact: true }).click()
    await page.getByRole("radio", { name: "已关闭", exact: true }).click()
    await page.getByRole("button", { name: "恢复并打开：项目群 236", exact: true }).click()
    await page.waitForURL("**#/c/chan:sp236")
    assert.equal(rows[236].closed, false)
    await page.getByRole("button", { name: "关闭当前对话" }).click()
    await page.waitForURL("**#/")
    assert.equal(rows[236].closed, true)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    // A write started under the old filter must refresh the current filter.
    await page.getByRole("button", { name: "打开群聊和私聊", exact: true }).click()
    await page.getByRole("radio", { name: "未关闭", exact: true }).click()
    await page.getByRole("textbox", { name: "搜索会话" }).fill("项目群 0")
    await page.getByRole("button", { name: "关闭：项目群 0", exact: true }).waitFor()
    delayed = true
    await page.getByRole("button", { name: "关闭：项目群 0", exact: true }).click()
    await page.getByRole("textbox", { name: "搜索会话" }).fill("项目群 235")
    await page.getByRole("button", { name: "打开：项目群 235", exact: true }).waitFor()
    releaseMutation(); delayed = false
    await page.getByRole("button", { name: "关闭：项目群 235", exact: true }).waitFor({ state: "visible" })
    await page.waitForFunction(() => !document.querySelector('[data-conversation-row="chan:sp235"] button').disabled)
    assert.equal(await page.locator("[data-conversation-row]").count(), 1)
    assert.equal(await page.getByRole("textbox", { name: "搜索会话" }).inputValue(), "项目群 235")
    // Unmount on workspace switch invalidates an outstanding restore/navigation.
    await page.getByRole("radio", { name: "已关闭", exact: true }).click()
    await page.getByRole("textbox", { name: "搜索会话" }).fill("项目群 0")
    delayed = true
    await page.getByRole("button", { name: "恢复并打开：项目群 0", exact: true }).click()
    await page.evaluate(() => window.dispatchEvent(new Event("fixture-workspace")))
    releaseMutation(); delayed = false
    await page.getByRole("button", { name: "打开群聊和私聊", exact: true }).waitFor()
    assert.equal(await page.locator("[data-conversation-manager]").count(), 0)
    assert.ok(page.url().endsWith("#/"))
    assert.deepEqual(errors, [])
    assertions += 25
    await page.close()
  }
  console.log(JSON.stringify({ passed: true, assertions, viewports: [390,1280], conversations:237 }))
} finally { await browser.close() }
