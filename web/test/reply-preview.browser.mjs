/* Production decision cards on a running Vite server, with synthetic channel data.
 * Checks presentation and real expand/collapse controls; no external messages are sent. */
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { settle } from "./settle.mjs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5180", shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const errors = []
const setPreview = (page, question, draft) => page.evaluate(({ question, draft }) => window.__preview(question, draft), { question, draft })
async function fits(page) {
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal page overflow")
  const over = await page.locator("[data-card-source], [data-card-draft]").evaluateAll((els) => els.flatMap((root) => {
    const bounds = root.getBoundingClientRect(), bad = []
    for (const child of root.querySelectorAll("*")) {
      const r = child.getBoundingClientRect()
      if (r.left < bounds.left - 1 || r.right > bounds.right + 1) bad.push(child.outerHTML.slice(0, 100))
      for (const node of child.childNodes) if (node.nodeType === Node.TEXT_NODE && node.textContent.trim()) {
        const range = document.createRange(); range.selectNodeContents(node)
        if ([...range.getClientRects()].some((r) => r.left < bounds.left - 1 || r.right > bounds.right + 1)) bad.push(node.textContent.slice(0, 100))
      }
    }
    return bad
  }))
  assert.deepEqual(over, [], "speaker and message text fit without clipping")
}
try {
  for (const [width, theme, lang, scale] of [[1280, "light", "zh-CN", 1], [1280, "dark", "zh-CN", 1], [390, "dark", "zh-CN", 1], [320, "light", "zh-CN", 1.5], [390, "light", "en", 1]]) {
    const page = await browser.newPage({ locale: lang, viewport: { width, height: 960 }, reducedMotion: "reduce" })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route(`${base}/api/**`, (route) => {
      if (new URL(route.request().url()).pathname === "/api/chan-messages/cm_out/context") return route.fulfill({ json: {
        space: { id: "sp1", name: "项目对齐群", kind: "group", external: false }, as_you: true, persona: null,
        text: "请审阅这条草稿", reply_to: { id: "cm_in", sender: "陈宁", text: "明天下午2点对齐方案，可以吗？\n第二行原话。", at: 1 },
        before: [{ sender: "王然", text: "方案已准备好", at: 0 }],
      } })
      return route.fulfill({ status: 503, json: { error: "fixture only" } })
    })
    const open = async (query = "") => {
      await page.goto(`${base}/test/fixtures/handoff-card.html?${query}${lang === "en" ? "&en" : ""}`)
      await page.evaluate(({ theme, scale }) => { document.documentElement.classList.toggle("dark", theme === "dark"); document.documentElement.style.setProperty("--type-scale", String(scale)) }, { theme, scale })
      await page.locator("[data-decision-id]").waitFor()
      await page.evaluate(() => document.fonts.ready)
      await settle(page)
    }
    await open()
    assert.equal(await page.locator("[data-card-source] [data-reply-sender]").textContent(), "李雷")
    assert.equal(await page.locator("[data-card-draft] [data-reply-text]").textContent(), "周四可以")
    const original = await page.locator("[data-card-source]").boundingBox(), reply = await page.locator("[data-card-draft]").boundingBox(), context = await page.locator("[data-card-context]").boundingBox()
    assert.ok(original.y + original.height <= reply.y && reply.y + reply.height <= context.y, "original, unsent reply, then explanation")
    assert.notEqual(await page.locator("[data-card-source]").evaluate((el) => getComputedStyle(el).backgroundColor), await page.locator("[data-card-draft]").evaluate((el) => getComputedStyle(el).backgroundColor), "distinct surfaces in each theme")
    await page.locator("[data-reply-history] summary").click()
    await page.locator("[data-reply-history] > div").waitFor({ state: "visible" })
    assert.match(await page.locator("[data-reply-history]").innerText(), /方案我晚点发/)
    await page.locator("[data-reply-history] summary").press("Enter")
    await page.locator("[data-reply-history] > div").waitFor({ state: "hidden" })
    assert.deepEqual(await page.evaluate(() => window.__calls), [], "viewing context answers nothing")
    await fits(page)
    if (shots) await page.screenshot({ path: `${shots}/reply-${width}-${theme}-${lang}.png`, fullPage: true, animations: "disabled" })

    // Long names/URLs, line breaks and literal markup stay readable and inert, including enlarged text.
    const name = "陈宁_" + "long-name".repeat(8), text = "明天下午2点对齐方案？ https://example.invalid/" + "segment".repeat(45)
    const draft = "明天下午2点我可以\n\n第二段保留换行。\n<script>window.__injected = true</script>"
    await setPreview(page, `「${"项目群".repeat(35)}」里 ${name} 找你：确认时间\n为什么要你：需要你决定\n最近的对话：\n→ ${name}：${text}`, draft)
    assert.equal(await page.locator("[data-card-source] [data-reply-text]").textContent(), text)
    assert.equal(await page.locator("[data-card-draft] [data-reply-text]").textContent(), draft)
    assert.equal(await page.evaluate(() => window.__injected), undefined)
    await fits(page)

    await setPreview(page, "旧格式问题\n原样保留的上下文：不能猜谁说的", "候选回复")
    assert.equal(await page.locator("[data-card-source]").count(), 0)
    assert.equal(await page.locator("[data-card-context]").textContent(), "原样保留的上下文：不能猜谁说的")
    await open("nodraft")
    assert.equal(await page.locator("[data-card-draft]").count(), 0)
    assert.equal(await page.locator('[data-option="send_draft"]').count(), 0)

    await open("approve")
    await page.locator("[data-out-reply-to]").waitFor()
    assert.equal(await page.locator("[data-out-reply-to] [data-reply-sender]").textContent(), "陈宁")
    assert.equal(await page.locator("[data-out-reply-to] [data-reply-text]").textContent(), "明天下午2点对齐方案，可以吗？\n第二行原话。")
    assert.equal(await page.locator("[data-card-draft] [data-reply-text]").textContent(), "请审阅这条草稿")
    await page.locator("[data-out-context] button").last().click()
    await page.locator("[data-out-before]").waitFor()
    assert.match(await page.locator("[data-out-before]").innerText(), /王然[：:]\s*方案已准备好/)
    await fits(page)
    if (shots) await page.screenshot({ path: `${shots}/approval-${width}-${theme}-${lang}.png`, fullPage: true, animations: "disabled" })
    assert.deepEqual(await page.evaluate(() => window.__calls), [])
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("reply-preview: ok (desktop/phone, light/dark, enlarged text, zh/en, context controls, legacy and approval)")
} finally { await browser.close() }
