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
const RAW = /\b(send_draft|instruct|verbatim|draft_reply|to_item|done|retry|discard|sent)\b/
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 844 } }), errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", (route) => { const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (u.pathname.startsWith("/api/")) return route.fulfill({ status: 503, json: { error: "fixture" } }); return route.continue() })
    const open = async (q = "") => { await page.goto(base + "/test/fixtures/handoff-card.html" + q); await page.locator("[data-decision-id]").first().waitFor() }
    const calls = () => page.evaluate(() => JSON.parse(JSON.stringify(window.__calls)))
    const card = page.locator("[data-decision-id]").first()
    /* what the card says, its hidden-until-typed send button and the field's placeholder included */
    const words = async () => card.evaluate((e) => [e.textContent, ...[...e.querySelectorAll("input,textarea")].map((x) => x.placeholder)].join(" ").replace(/\s+/g, " "))

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

    await open()
    await card.locator('[data-option="to_item"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
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
    if (shots) await page.screenshot({ path: `${shots}/handoff-en-${width}.png` })

    for (const [q, want] of [["?fail", ["重新发送", "不发了"]], ["?unknown", ["已经发出了", "重新发送", "不发了"]], ["?blocked", ["不发了"]]]) {
      await open(q)
      text = await words()
      for (const w of want) assert.ok(text.includes(w), `${q}: ${w} in ${text}`)
      assert.ok(!text.includes("重试发布"), text)
      assert.doesNotMatch(text, RAW)
    }
    await card.locator('[data-fail="discard"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_f", choice: "discard" }])
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log("handoff-card ok")
} finally { await browser.close() }
