import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
/* a message handed over to you shows its options by their labels and answers with the Hub's own option ids: 发这条草稿 /
 * 按原文发 / 转事项 / 不用管 as buttons, how to reply as the field (instruct), your words as written with 按原文发. A reply that didn't go out offers every option
 * the Hub gave it (重新发送, 不发了, 已经发出了), each by its label. Mocked API, no Hub. */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5196"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })
const RAW = /\b(send_draft|instruct|verbatim|rewrite|draft_reply|to_item|done|retry|discard|sent)\b/
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 844 } }), errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    let status = "failed", readError = false, paged = false, recordReads = 0
    await page.route("**/*", async (route) => {
      const u = new URL(route.request().url())
      if (u.origin !== new URL(base).origin) return route.abort()
      if (u.pathname === "/api/chan-messages/cm_out/context") return route.fulfill({ json: {
        space: { id: "sp1", name: "运维群", kind: "group", external: false }, as_you: false, persona: "Steve",
        text: await page.evaluate(() => window.__body), reply_to: null, before: [],
      } })
      if (decodeURIComponent(u.pathname) === "/api/conversations/chan:sp1/record") {
        recordReads++
        if (readError) return route.fulfill({ status: 503, json: { error: "fixture readback failed" } })
        if (paged && !u.searchParams.has("before")) return route.fulfill({ json: { records: [{ id: "other", status: "sent", text: "别的消息" }], has_more: true, next: { before: 100, before_id: "older" } } })
        return route.fulfill({ json: { records: [{ id: "cm_out", status, text: await page.evaluate(() => window.__body), sent: status === "sent" ? 1800000000000 : null,
          open_url: "https://applink.feishu.cn/client/chat/open?openChatId=fixture", out: { status, decision: "dec_f", error: ["failed", "unknown"].includes(status) ? "渠道服务拒绝：target_forbidden (-32016)" : null } }], has_more: false, next: null } })
      }
      if (u.pathname.startsWith("/api/")) return route.fulfill({ status: 503, json: { error: "fixture" } })
      return route.continue()
    })
    const open = async (q = "") => {
      readError = false; paged = false; recordReads = 0; status = q.includes("unknown") ? "unknown" : "failed"
      await page.goto(base + "/test/fixtures/handoff-card.html" + q)
      await page.locator("[data-decision-id]").first().waitFor()
      if (q.includes("blocked") || q.includes("fail") || q.includes("unknown")) await page.locator(`[data-send-status="${status}"]`).waitFor()
    }
    const calls = () => page.evaluate(() => JSON.parse(JSON.stringify(window.__calls)))
    const card = page.locator("[data-decision-id]").first()
    const words = async () => card.evaluate((e) => [e.textContent, ...[...e.querySelectorAll("input,textarea")].map((x) => x.placeholder)].join(" ").replace(/\s+/g, " "))
    const noOverflow = async () => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no horizontal overflow at ${width}`)
    const click = async (selector) => { await card.locator(selector).click(); await page.waitForFunction(() => window.__calls.length === 1) }

    await open()
    let text = await words()
    /* the question is the first line; why you, the lines and the candidate draft are its detail */
    assert.ok(text.includes("李雷问周四下午能不能一起过一下方案") && text.includes("为什么要你") && text.includes("→ 李雷：周四下午能过一下吗"), text)
    assert.ok(text.includes("可以直接发的草稿") && text.includes("周四可以"), text)
    assert.ok(["发这条草稿", "按原文发", "转事项", "不用管", "告诉它怎么回"].every((x) => text.includes(x)), text)
    assert.doesNotMatch(text, RAW)
    assert.equal(await card.locator("[data-card-context]").count(), 1)
    if (shots) await page.screenshot({ path: `${shots}/handoff-${width}.png` })
    await card.locator('[data-option="send_draft"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "send_draft" }])

    await open()
    await card.locator('[data-option="done"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "done" }])
    await page.locator("[data-closed]").waitFor()

    await open()
    await click('[data-option="to_item"]')
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "to_item" }])

    /* the field: how to reply goes to the composer… */
    await open()
    await card.locator('[data-option-words="instruct"]').fill("跟他说周四下午三点")
    assert.equal((await card.locator('[data-option="instruct"]').innerText()).trim(), "告诉它怎么回")
    if (shots) await page.screenshot({ path: `${shots}/handoff-typed-${width}.png` })
    await card.locator('[data-option="instruct"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "instruct", text: "跟他说周四下午三点" }])

    /* …or, with 按原文发, goes out as you wrote it */
    await open()
    await card.locator('[data-option-words="instruct"]').fill("周四下午三点")
    await card.locator('[data-option="verbatim"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "verbatim", text: "周四下午三点" }])

    /* without a candidate draft there is nothing to send as is */
    await open("?nodraft")
    assert.equal(await card.locator('[data-option="send_draft"]').count(), 0)
    assert.equal(await card.locator("[data-card-draft]").count(), 0)

    /* a card opened before: its reply field as it was */
    await open("?legacy")
    await card.locator('[data-option-words="draft_reply"]').fill("我晚点回你")
    assert.equal((await card.locator('[data-option="draft_reply"]').innerText()).trim(), "起草回复")
    await card.locator('[data-option="draft_reply"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "draft_reply", text: "我晚点回你" }])

    await open("?en")
    text = await words()
    assert.ok(text.includes("Send this draft") && text.includes("Make it an item") && text.includes("Leave it") && text.includes("Tell it how to reply"), text)
    assert.doesNotMatch(text, RAW)

    // Normal approval still asks the model for a rewrite, without the failed-send edit contract.
    for (const [selector, choice, note] of [['[data-approve]', "yes"], ['[data-rewrite]', "rewrite"], ['[data-reject]', "no", "请补充依据"]]) {
      await open("?approve")
      assert.equal(await page.locator("[data-send-recovery]").count(), 0)
      if (note) await card.locator("[data-reject-reason]").fill(note)
      await click(selector)
      assert.deepEqual(await calls(), [{ id: "dec_a", choice, ...(note ? { text: note } : {}) }])
    }

    for (const [q, want] of [["?fail", ["重新发送", "不发了"]], ["?unknown", ["已经发出了", "重新发送", "不发了"]], ["?blocked-legacy", ["不发了"]], ["?blocked", ["授权后重试", "修改回复", "不发了"]]]) {
      await open(q)
      text = await words()
      for (const word of want) assert.ok(text.includes(word), `${q}: ${word}`)
      assert.ok(!text.includes("重试发布"), text)
      assert.doesNotMatch(text.replace(/target_forbidden/g, ""), RAW)
      assert.equal(await card.locator("[data-send-body]").textContent(), await page.evaluate(() => window.__body), "full exact text, line breaks and last line preserved")
      assert.equal(await page.evaluate(() => window.__injected), undefined, "proposed body is inert text")
      await noOverflow()
    }
    assert.ok((await card.locator("[data-send-block]").innerText()).includes("不会授予外部权限"))
    assert.ok((await card.locator("[data-send-reason]").innerText()).includes("-32016"))
    assert.ok((await card.locator("[data-send-source]").innerText()).includes("cm_out"))
    if (shots) await page.screenshot({ path: `${shots}/blocked-${width}.png`, fullPage: true })

    // The review input is prefilled, multiline, and an unsuccessful submission retains edits.
    await card.locator('[data-fail="rewrite"]').click()
    assert.equal(await card.locator("[data-send-edit]").inputValue(), await page.evaluate(() => window.__body))
    assert.deepEqual(await calls(), [], "opening the editor sends nothing")
    const revised = "修改后的第一行\n\n第二段：仍然需要批准。\n保留末尾换行\n"
    await card.locator("[data-send-edit]").fill(" \n ")
    assert.equal(await card.locator("[data-send-review]").isDisabled(), true)
    await card.locator("[data-send-edit]").fill(revised)
    assert.ok((await card.locator("[data-send-editor]").innerText()).includes("不会立即发送"))
    await noOverflow()
    if (shots) await page.screenshot({ path: `${shots}/blocked-edit-${width}.png`, fullPage: true })
    await page.evaluate(() => { window.__decisionError = true })
    await click("[data-send-review]")
    await page.waitForFunction(() => !document.querySelector("[data-send-edit]").disabled)
    assert.equal(await card.locator("[data-send-edit]").inputValue(), revised)
    assert.deepEqual(await calls(), [{ id: "dec_f", choice: "rewrite", text: revised }])
    await page.evaluate(() => { window.__decisionError = false; window.__calls.length = 0 })
    await click("[data-send-review]")
    assert.deepEqual(await calls(), [{ id: "dec_f", choice: "rewrite", text: revised }], "modification is review, never yes/retry")
    await page.locator("[data-closed]").waitFor()

    await open("?blocked")
    await card.locator('[data-fail="rewrite"]').click()
    await card.locator("[data-send-edit]").fill("取消掉的修改")
    await card.locator("[data-send-cancel-edit]").click()
    assert.deepEqual(await calls(), [])
    await click('[data-fail="discard"]')
    assert.deepEqual(await calls(), [{ id: "dec_f", choice: "discard" }])
    await page.locator("[data-closed]").waitFor()

    // Two clicks and competing actions while busy produce one request. Accepted + stale/failed state stays locked.
    await open("?blocked&hold")
    await page.evaluate(() => { window.__stateError = true })
    await card.locator('[data-fail="retry"]').dblclick({ force: true })
    await page.waitForFunction(() => window.__calls.length === 1 && typeof window.__release === "function")
    assert.equal(await card.locator('[data-fail="retry"]').locator("xpath=ancestor::button").getAttribute("aria-disabled"), "true")
    assert.equal(await card.locator('[data-fail="rewrite"]').isDisabled(), true)
    await card.locator('[data-fail="discard"]').click({ force: true })
    assert.equal((await calls()).length, 1)
    status = "pending"
    await page.evaluate(() => window.__release())
    await page.waitForFunction(() => document.querySelector("[data-send-submitting]")?.textContent.includes("已收到处理请求"))
    await page.locator('[data-send-status="pending"]').waitFor()
    await card.locator('[data-fail="retry"]').click({ force: true })
    assert.deepEqual(await calls(), [{ id: "dec_f", choice: "retry" }])
    status = "sent"
    await page.evaluate(() => window.__reload())
    await page.locator('[data-send-status="sent"]').waitFor()
    assert.ok((await card.locator("[data-send-receipt]").innerText()).includes("未提供外部渠道回执"))
    await page.evaluate(() => { window.__stateError = false })
    await card.locator("[data-send-refresh]").click()
    await page.locator("[data-closed]").waitFor()

    await open("?unknown")
    assert.ok((await card.locator("[data-send-status]").innerText()).includes("避免重复发送"))
    assert.equal(await card.locator("[data-send-stale]").count(), 0, "the current unknown-result card remains actionable")
    await click('[data-fail="sent"]')
    assert.deepEqual(await calls(), [{ id: "dec_f", choice: "sent" }])

    await open("?blocked")
    status = "pending"
    await page.evaluate(() => window.__reload())
    await card.locator('[data-send-status="pending"]').waitFor()
    assert.equal(await card.locator('[data-fail="retry"]').locator("xpath=ancestor::button").getAttribute("aria-disabled"), "true", "a send started elsewhere disables the old card")
    await card.locator('[data-fail="retry"]').click({ force: true })
    assert.deepEqual(await calls(), [])
    status = "failed"
    await page.evaluate(() => window.__reload())
    await card.locator('[data-send-status="failed"]').waitFor()
    await click('[data-fail="discard"]')
    assert.deepEqual(await calls(), [{ id: "dec_f", choice: "discard" }])

    // Status failures are visible; older records are paged by their own id, then manual refresh recovers.
    await open("?blocked-legacy")
    readError = true
    await card.locator("[data-send-refresh]").click()
    await page.locator("[data-send-read-error]").waitFor()
    assert.ok((await card.locator("[data-send-source]").innerText()).includes("dec_f"))
    assert.equal(await card.locator("[data-send-body]").textContent(), await page.evaluate(() => window.__body))
    assert.equal(await card.locator('[data-fail="retry"]').count(), 0, "old Hub options are not invented")
    assert.equal(await card.locator('[data-fail="rewrite"]').count(), 0)
    readError = false; paged = true; const before = recordReads
    await card.locator("[data-send-refresh]").click()
    await page.locator('[data-send-status="failed"]').waitFor()
    assert.ok(recordReads >= before + 2, "read the older page containing this message")

    await open("?blocked&en")
    text = await words()
    for (const word of ["Full proposed message", "Retry after restoring access", "Edit reply", "Don't send it", "does not grant external permissions"]) assert.ok(text.includes(word), word)
    await noOverflow()
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("handoff-card ok — mocked API, desktop 1280 and mobile 390; not live send acceptance")
} finally { await browser.close() }
