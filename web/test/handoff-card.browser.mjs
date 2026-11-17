import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
/* a message handed over to you shows its options by their labels and answers with the Hub's own option ids: 转成事项 /
 * 不用回复 as buttons, the reply you write as draft_reply with its text. A reply that didn't go out offers every option
 * the Hub gave it (重新发送, 不发了, 已经发出了), each by its label. Mocked API, no Hub. */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5196"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) mkdirSync(shots, { recursive: true })
const RAW = /\b(draft_reply|to_item|done|retry|discard|sent)\b/
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
    assert.ok(text.includes("转成事项") && text.includes("不用回复") && text.includes("起草回复") && text.includes("写下要回的话"), text)
    assert.doesNotMatch(text, RAW)
    if (shots) await page.screenshot({ path: `${shots}/handoff-${width}.png` })
    await card.locator('[data-option="done"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "done" }])

    await open()
    await card.locator('[data-option="to_item"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "to_item" }])

    await open()
    await card.locator('[data-option-words="draft_reply"]').fill("我晚点回你")
    assert.equal((await card.locator('[data-option="draft_reply"]').innerText()).trim(), "起草回复")
    if (shots) await page.screenshot({ path: `${shots}/handoff-typed-${width}.png` })
    await card.locator('[data-option="draft_reply"]').click()
    await page.waitForFunction(() => window.__calls.length === 1)
    assert.deepEqual(await calls(), [{ id: "dec_h", choice: "draft_reply", text: "我晚点回你" }])

    await open("?en")
    text = await words()
    assert.ok(text.includes("Make it an item") && text.includes("No reply needed") && text.includes("Draft reply"), text)
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
