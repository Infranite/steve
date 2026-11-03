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
    const page = await browser.newPage({ viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
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
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "keyboard actions", "steps disclosure", "draft reload", "slash menu", "steady header", "long Markdown overflow", "rail/sheet", "thinking and stop", "empty conversation"] }))
} finally { await browser.close() }
