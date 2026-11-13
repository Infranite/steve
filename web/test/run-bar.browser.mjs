// Vite-only fixture; external Playwright prerequisite, no API calls.
// The run bar while a very long command runs, and the timeline of an interrupted turn: its pill, its totals, its rows.
import assert from "node:assert/strict"
import { settle } from "./settle.mjs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
const W = { "zh-CN": { stopped: "已中断", think: "思考", run: /^运行 cargo test/, read: /读取 workflows\.rs/ }, en: { stopped: "Interrupted", think: "Thinking", run: /^Run cargo test/i, read: /Read workflows\.rs/ } }
try {
  for (const [width, dark, lang] of [[1440, false, "zh-CN"], [1440, true, "en"], [390, false, "en"], [390, true, "zh-CN"]]) {
    const w = W[lang]
    const page = await browser.newPage({ locale: lang, viewport: { width, height: 1000 } })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.addInitScript((l) => localStorage.setItem("steve-lang", l), lang)
    await page.route("**/*", (route) => new URL(route.request().url()).origin !== base || new URL(route.request().url()).pathname.startsWith("/api/") ? route.abort("blockedbyclient") : route.continue())
    await page.goto(`${base}/test/fixtures/run-bar.html${dark ? "?dark" : ""}`)

    /* running: the step under way is the label, cut off with an ellipsis; one live region, not two */
    const bar = page.locator('[data-runbar="run"]')
    await bar.waitFor()
    const label = bar.locator(".ui-tlc__label:not(.ui-tlc__label--out)")
    assert.match(await label.textContent(), w.run)
    const cut = await label.evaluate((x) => ({ over: x.scrollWidth > x.clientWidth, ellipsis: getComputedStyle(x).textOverflow, right: x.getBoundingClientRect().right }))
    assert.equal(cut.over, true, "a long command overflows its label")
    assert.equal(cut.ellipsis, "ellipsis")
    assert.ok(cut.right <= width, `label ends inside the viewport: ${cut.right}`)
    assert.equal(await bar.locator('[aria-live]').count(), 1)
    assert.equal(await bar.getAttribute("aria-live"), null)
    assert.equal(await bar.locator("[data-interrupt]").isVisible(), true)

    /* interrupted: the steps say so, and so does the timeline – never a green "Done" */
    const work = page.locator('[data-fixture="stopped"] [data-work="done"]')
    await work.locator("> button").click()
    assert.equal(await page.locator('[data-fixture="stopped"] [data-trail-end="cancel"]').count(), 1)
    await work.locator('[data-seg="timeline"] >> visible=true').click()
    const trace = page.locator('[data-fixture="stopped"] [data-turn-trace="cancelled"]')
    await trace.waitFor()
    await settle(page)
    const pill = trace.locator(".ui-trwc__bar-head .ui-trwc__pill")
    assert.equal((await pill.textContent()).trim(), w.stopped)
    assert.equal(await pill.getAttribute("data-status"), "cancelled")
    /* the totals: thinking counts only the thinking (the note is not one), the duration fits its column */
    const stats = Object.fromEntries(await trace.locator(".ui-trwc__stat").evaluateAll((xs) => xs.map((x) => [x.querySelector("dt").textContent, x.querySelector("dd").textContent])))
    assert.equal(stats[w.think], "1", JSON.stringify(stats))
    for (const dd of await trace.locator(".ui-trwc__stat dd").all()) {
      const [sw, cw, text] = await dd.evaluate((x) => [x.scrollWidth, x.clientWidth, x.textContent])
      assert.ok(sw <= cw, `stat "${text}" fits: ${sw} > ${cw}`)
    }
    assert.match(await trace.locator(".ui-trwc__stat dd").first().getAttribute("title"), /\d/)
    /* rows say what the steps say: Steve's verb next to the tool's name */
    const read = trace.locator('[role="treeitem"]', { hasText: "Read" }).first()
    assert.match(await read.locator(".ui-trwc__detail").textContent(), w.read)
    /* Steve's own tool: what went in, as the Hub stored it */
    await trace.locator('[role="treeitem"]', { hasText: "steve_workflow" }).first().click()
    assert.match(await trace.locator(".ui-trwc__pop").innerText(), /expected_revision[\s\S]*37/)

    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["long running command ellipsis", "one live region", "interrupted timeline pill", "thinking total", "duration fits", "steve verb on rows", "steve tool input", "1440/390px", "light/dark", "zh-CN/en"] }))
} finally { await browser.close() }
