// Vite-only fixture; external Playwright prerequisite, no API calls.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
try {
  for (const width of [1066, 390]) for (const theme of ["dark", "light"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 700 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", e => errors.push(e.message))
    await page.goto(base + "/test/fixtures/item-rows.html")
    await page.locator('[data-item-row="ST-53"]').waitFor()
    const row = id => page.locator(`[data-item-row="${id}"]`)
    // waiting for your word without a card: the summary is the reason, no button to a card that isn't there
    assert.equal(await row("ST-49").locator("[data-item-now]").innerText(), "新周期，等你说要做什么")
    assert.equal(await row("ST-49").locator("[data-item-wait-go]").count(), 0)
    // running: what it does now
    assert.equal(await row("ST-48").locator("[data-item-now]").innerText(), "新周期，收到新消息，开始处理")
    assert.equal(await row("ST-5").locator('[data-workflow-relation="parent"]').innerText(), "等待 ST-26")
    assert.equal(await row("ST-10").locator('[data-workflow-relation="paused"]').innerText(), "协作已暂停 · ST-43 失败")
    assert.equal(await row("ST-10").locator('[data-item-now]').count(), 0, "a paused workflow does not repeat a stale running summary")
    assert.equal(await row("ST-10").getAttribute("data-item-status"), "pause")
    assert.equal(await row("ST-44").locator('[data-workflow-relation="previous"]').innerText(), "旧周期协作记录 · 当前周期待处理")
    assert.equal(await row("ST-44").getAttribute("data-item-status"), "run", "a prior cycle's pause must not pause the current item")
    assert.equal(await row("ST-44").locator('[data-item-now]').count(), 0, "a prior cycle's summary is not current work")
    assert.equal(await row("ST-26").locator('[data-workflow-relation="child"]').innerText(), "协作子项 · ST-5")
    assert.equal(await row("ST-27").locator("[data-workflow-relation]").count(), 0, "a completed workflow child does not keep its parent waiting")
    assert.equal(await row("ST-28").locator("[data-workflow-relation]").count(), 0, "an ordinary fork is not a workflow child")
    const rel31 = row("ST-31").locator('[data-workflow-relation="child"]')
    assert.equal(await rel31.evaluate((el) => el.scrollWidth <= el.clientWidth), true, `a long summary does not squeeze the relation away at ${width}px`)
    // a truncated part keeps the dot after it, and the dot sits between the parts, never over one
    for (const id of ["ST-31", "ST-18", "ST-26"]) {
      const seps = await row(id).evaluate((el) => [...el.querySelectorAll("[data-meta-sep]")].filter((s) => s.getBoundingClientRect().width > 0).map((s) => {
        const b = s.getBoundingClientRect(), p = s.previousElementSibling.getBoundingClientRect(), n = s.nextElementSibling.getBoundingClientRect(), m = s.parentElement.getBoundingClientRect()
        return b.left >= p.right - 0.5 && b.right <= n.left + 0.5 && b.left >= m.left && b.right <= m.right
      }))
      assert.deepEqual(seps, [true, true], `${id}: both separators show between their parts at ${width}px`)
    }
    assert.equal(await row("ST-43").getAttribute("data-dense"), "true", "an item closed as failed folds like other closed ones")
    // a real wait: what the card asks and the way to it, not the stale summary
    const r = row("ST-52")
    assert.match(await r.locator('[data-item-wait="approve"]').innerText(), /^审批：合并 PR #12 并打 tag 发版 等 2 件$/)
    assert.equal(await r.locator("[data-item-now]").count(), 0)
    assert.equal((await r.locator("[data-item-wait-go]").innerText()).trim(), "去审批")
    assert.equal((await row("ST-53").locator("[data-item-wait-go]").innerText()).trim(), "去回答")
    // nothing reaches past the row
    for (const id of ["ST-49", "ST-48", "ST-52", "ST-53", "ST-5", "ST-26", "ST-31"]) {
      const over = await row(id).evaluate(el => { const b = el.getBoundingClientRect(); return [...el.querySelectorAll("*")].filter(e => e.getBoundingClientRect().right > b.right + 1).map(e => e.outerHTML.slice(0, 80)) })
      assert.deepEqual(over, [], `${id} overflows at ${width}px`)
    }
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/item-rows-${width}-${theme}.png`, fullPage: true })
    // the button takes you to the item, where its card is
    await r.locator("[data-item-wait-go]").click()
    assert.equal(new URL(page.url()).hash, "#/items/ST-52")
    await page.close()
    const grouped = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 700 }, reducedMotion: "reduce", colorScheme: theme })
    grouped.on("pageerror", e => errors.push(e.message))
    await grouped.goto(base + "/test/fixtures/item-rows.html?groups")
    await grouped.locator('[data-items-group="pause"] [data-item-row="ST-10"]').waitFor()
    assert.equal(await grouped.locator('[data-items-group="run"] [data-item-row="ST-44"]').count(), 1)
    assert.equal(await grouped.locator('[data-items-group="run"] [data-item-row="ST-10"]').count(), 0)
    assert.equal(await grouped.locator('[data-items-group="fail"] [data-item-row="ST-43"]').count(), 1, "failed items have their own group")
    // the board stays in its own area: it may scroll sideways there, the page never does
    await grouped.locator("[data-seg=board]:visible").click()
    const board = grouped.locator("[data-items-board]")
    await board.waitFor()
    const fit = await board.evaluate((el) => ({ right: el.getBoundingClientRect().right, scrolls: el.scrollWidth > el.clientWidth, more: el.dataset.more ?? "" }))
    assert.ok(fit.right <= width + 0.5, `board reaches past the window at ${width}px`)
    assert.equal(fit.scrolls ? /end/.test(fit.more) : true, true, "a board wider than its area says there is more")
    assert.equal(await grouped.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, `page scrolls sideways with the board at ${width}px`)
    await grouped.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["new cycle waiting for your word", "running after take-up", "approval reason and entry", "question entry", "no overflow", "board in its own area", "1066/390px", "dark/light"] }))
} finally { await browser.close() }
