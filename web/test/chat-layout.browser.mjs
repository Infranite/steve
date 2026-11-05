import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const errors = []
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    await page.goto(base + "/test/fixtures/chat-layout.html")
    await page.locator('[data-msg="answer"]').waitFor()
    await page.waitForTimeout(400)
    // Keyboard users must see the action they have focused, without requiring hover.
    const actions = page.locator('[data-msg="answer"] [data-msg-actions]')
    await actions.getByRole("button", { name: "复制", exact: true }).focus()
    await page.waitForFunction(() => {
      const actions = document.querySelector('[data-msg="answer"] [data-msg-actions]')
      return actions && getComputedStyle(actions).opacity === "1"
    }, null, { timeout: 3000 })
    const steps = page.locator('[data-steps="answer"]')
    await steps.locator("[data-steps-toggle]").click()
    await steps.locator("[data-step-route]").waitFor()
    assert.equal(await steps.locator("[data-steps-toggle]").getAttribute("aria-expanded"), "true")
    await steps.locator("[data-steps-toggle]").click()
    const input = page.locator('[data-composer="chat"] textarea')
    await input.fill("对话样式验收草稿")
    await page.reload()
    assert.equal(await input.inputValue(), "对话样式验收草稿")
    await input.fill("/")
    await page.locator('[data-ref-menu="/"]').waitFor()
    await input.press("Escape")
    await input.fill("")
    await page.mouse.click(width / 2, 32)
    await page.waitForFunction(() => {
      const expected = getComputedStyle(document.body).color
      return [...document.querySelectorAll("[data-hero] .type-headline, [data-me-msg] .md")].every(el => getComputedStyle(el).color === expected)
    }, null, { timeout: 3000 })
    await page.waitForTimeout(250)
    if (shots) await page.screenshot({ path: `${shots}/chat-${width}-${theme}.png`, animations: "disabled" })
    // A long reply may scroll inside its code/table, but never widen the document.
    await page.goto(base + "/test/fixtures/chat-layout.html?long")
    await page.locator('[data-msg="long"]').waitFor()
    await page.waitForTimeout(500)
    const header = page.locator("[data-chat-header]")
    const before = await header.boundingBox()
    const scroller = page.locator("[data-chat-scroll]")
    await scroller.evaluate(el => { el.scrollTop = 0 })
    await page.waitForTimeout(350)
    assert.deepEqual(await header.boundingBox(), before)
    assert.equal(await scroller.evaluate(el => getComputedStyle(el).maskImage), "none")
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    const box = await page.locator("[data-chat-input]").boundingBox()
    assert.ok(box.x >= 0 && box.x + box.width <= width)
    await scroller.evaluate(el => { el.scrollTop = el.scrollHeight })
    await page.waitForTimeout(350)
    if (shots) await page.screenshot({ path: `${shots}/chat-long-${width}-${theme}.png`, animations: "disabled" })
    // At the bottom, new messages follow, even after a wheel that could not scroll any further down.
    const atBottom = () => scroller.evaluate(el => el.scrollHeight - el.scrollTop - el.clientHeight < 2)
    const newMsg = page.locator("[data-new-messages]")
    await page.mouse.move(width / 2, 300)
    await page.mouse.wheel(0, 400)
    await page.waitForTimeout(200)
    await page.evaluate(() => window.dispatchEvent(new Event("fixture-reply")))
    await page.locator('[data-msg="reply-1"]').waitFor()
    await page.waitForTimeout(300)
    assert.ok(await atBottom())
    assert.equal(await newMsg.count(), 0)
    if (shots) await page.screenshot({ path: `${shots}/chat-follow-${width}-${theme}.png`, animations: "disabled" })
    // The composer growing shrinks the list; the latest message stays in view.
    await page.locator("[data-chat-input]").evaluate(el => { el.style.paddingTop = "160px" })
    await page.waitForTimeout(200)
    assert.ok(await atBottom())
    await page.locator("[data-chat-input]").evaluate(el => { el.style.paddingTop = "" })
    // Scrolled up to read: the view stays put and offers the way down.
    await page.mouse.wheel(0, -500)
    await page.waitForTimeout(300)
    const top = await scroller.evaluate(el => el.scrollTop)
    await page.evaluate(() => window.dispatchEvent(new Event("fixture-reply")))
    await page.locator('[data-msg="reply-2"]').waitFor()
    await page.waitForTimeout(300)
    assert.equal(await scroller.evaluate(el => el.scrollTop), top)
    assert.equal((await newMsg.textContent()).trim(), "1 条新消息")
    if (shots) await page.screenshot({ path: `${shots}/chat-unread-${width}-${theme}.png`, animations: "disabled" })
    await newMsg.click()
    await page.waitForTimeout(200)
    assert.ok(await atBottom())
    assert.equal(await newMsg.count(), 0)
    // A sideways swipe never scrolls the conversation or the rail, even when something in them is a little too wide.
    await page.goto(base + "/test/fixtures/chat-layout.html?rich")
    await page.locator('[data-item-card="ST-47"]').waitFor()
    await page.waitForTimeout(400)
    if (shots) await page.screenshot({ path: `${shots}/chat-rich-${width}-${theme}.png`, animations: "disabled" })
    assert.ok(await page.locator("[data-notice-line]").evaluate(el => el.scrollWidth <= el.clientWidth), "a long link in a notice wraps")
    for (const sel of ["[data-chat-scroll]", ...(width === 1440 ? ['[data-rail="aside"]'] : [])]) {
      const pane = page.locator(sel)
      await pane.evaluate(el => { const w = document.createElement("div"); w.style.width = el.clientWidth + 40 + "px"; w.style.height = "1px"; el.firstElementChild.append(w) })
      await pane.hover()
      await page.mouse.wheel(120, 0)
      await page.waitForTimeout(150)
      assert.equal(await pane.evaluate(el => el.scrollLeft), 0, sel)
    }
    if (width === 390) {
      await page.locator("[data-rail-toggle]").click()
      await page.locator('[data-rail="sheet"]').waitFor()
      assert.ok(await page.getByRole("heading", { name: "通知和摘要" }).count())
      await page.keyboard.press("Escape")
    } else {
      await page.locator("[data-rail-toggle]").click()
      assert.equal(await page.locator('[data-rail="aside"]').count(), 0)
    }
    await page.evaluate(() => window.dispatchEvent(new Event("fixture-thinking")))
    await page.locator("[data-live-steps]").waitFor()
    await page.locator("[data-stop]").waitFor()
    await page.goto(base + "/test/fixtures/chat-layout.html?empty")
    await page.locator('[data-composer="chat"]').waitFor()
    assert.equal(await page.locator("[data-chat-author]").count(), 0)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "keyboard actions", "steps disclosure", "draft reload", "slash menu", "steady header", "long Markdown overflow", "follow new messages at the bottom", "no sideways scroll", "rail/sheet", "thinking and stop", "empty conversation"] }))
} finally { await browser.close() }
