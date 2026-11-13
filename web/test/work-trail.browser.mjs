// Vite-only fixture; external Playwright prerequisite, no API calls.
// The work trail, in an agent's run and in Steve's conversation: calls of one kind merge into one line with chips, a
// step's verb follows its state, thinking streams while it runs and folds after, the latest plan stays open, the end is checked.
import assert from "node:assert/strict"
import { settle } from "./settle.mjs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
/* a line's words; a tool call shown as a Tool Call Card says them in its summary (its name and status sit beside it) */
const lines = (loc) => loc.locator(":scope > li").evaluateAll((xs) => xs.map((x) => ((x.matches("[data-tool-card]") ? x.querySelector(".ui-tcl__summary") : x.querySelector("button, div"))?.textContent ?? "").trim()))
try {
  for (const [width, dark] of [[1280, false], [1280, true], [390, false]]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 1100 } })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", (route) => new URL(route.request().url()).origin !== base || new URL(route.request().url()).pathname.startsWith("/api/") ? route.abort("blockedbyclient") : route.continue())

    /* live: the run's trail ends on the call running now, with a spinner and the present tense */
    await page.goto(`${base}/test/fixtures/work-trail.html${dark ? "?dark" : ""}`)
    const run = page.locator('[data-fixture="run"] [data-work="live"] [data-trail]')
    await run.waitFor()
    const live = await lines(run)
    assert.match(live.at(-1), /^正在运行 · 第 2 条命令/, JSON.stringify(live))
    assert.equal(await run.locator(':scope > li:last-child > [data-trail-marker="run"]').count(), 1)
    assert.ok(live.some((l) => /^读取了 5 个文件/.test(l)), JSON.stringify(live))
    assert.ok(live.some((l) => /^交给了品牌研究员/.test(l)), JSON.stringify(live))
    assert.equal(await run.locator('[data-plan-step="run"]').count(), 1)
    assert.equal(await run.locator("[data-trail-end]").count(), 0)
    /* a single call is a Tool Call Card: the tool's own name, Steve's words as its summary, and opened what went in */
    const task = run.locator('[data-tool-card="Task"]')
    assert.equal(await task.locator(".ui-tcl__name").textContent(), "Task")
    await task.locator(".ui-tcl__head").click()
    await settle(page)
    assert.match(await task.locator(".ui-tcl__body").innerText(), /参数[\s\S]*品牌研究员：扫描瑞典竞品品牌/)

    /* the conversation: research folded open, merged lines with their briefs as chips, the failed call marked, a check at the end */
    const steps = page.locator("[data-fixture-steps] [data-trail]")
    await steps.waitFor()
    const done = await lines(steps)
    const want = [/^分诊就地研究/, /^读取了 3 个文件/, /^搜索了 2 次/, /^执行git log/, /^查知识库/, /^完成/]
    assert.equal(done.length, want.length, JSON.stringify(done))
    want.forEach((w, n) => assert.match(done[n], w, JSON.stringify(done)))
    assert.equal(await steps.locator('[data-step-group="read"] [data-trail-chips] > span').count(), 3)
    assert.equal(await steps.locator('[data-trail-marker="fail"]').count(), 1)
    assert.equal(await steps.locator('[data-trail-end="end"]').count(), 1)
    /* a merged line opens to each call */
    await steps.locator('[data-step-group="read"] button').first().click()
    assert.equal(await steps.locator('[data-step-group="read"] ol > li').count(), 3)

    /* research under way and thinking streaming: present tense, a clock */
    assert.match(await page.locator("[data-research-live]").textContent(), /正在工作.*正在搜索 · 第 2 次/)
    assert.match(await page.locator("[data-fixture-think] [data-thinking]").textContent(), /正在思考\d+ 秒/)

    /* finished: thinking folds to its time, the trail closes with a check */
    await page.goto(`${base}/test/fixtures/work-trail.html?done${dark ? "&dark" : ""}`)
    await page.locator('[data-fixture="run"] [data-work="done"] > button').click()
    const fin = page.locator('[data-fixture="run"] [data-work="done"] [data-trail]')
    await fin.waitFor()
    const f = await lines(fin)
    assert.match(f[0], /^思考了 6 秒/, JSON.stringify(f))
    assert.match(f.at(-1), /^完成/, JSON.stringify(f))
    assert.equal(await fin.locator('[data-plan-step="run"]').count(), 0)
    await fin.locator('[data-step-kind="th"] button').click()
    await fin.locator("[data-thought-body]").waitFor()

    /* the same turn on a timeline: the turn and every step as a bar, no cost or tokens where the Hub has none */
    await page.locator('[data-fixture="run"] [data-work="done"] [data-seg="timeline"] >> visible=true').click()
    const trace = page.locator('[data-fixture="run"] [data-turn-trace="done"]')
    await trace.waitFor()
    assert.equal(await trace.locator('[role="treeitem"]').count(), 17)
    assert.equal(await trace.locator('[role="treeitem"][data-kind="subagent"]').count(), 1)
    assert.equal(await trace.locator('[role="treeitem"][data-kind="llm"]').count(), 2)
    assert.deepEqual(await trace.locator(".ui-trw__stat dt").allTextContents(), ["用时", "思考", "工具调用", "失败"])
    assert.equal((await trace.locator(".ui-trw__bar-head .ui-trw__pill").textContent()).trim(), "完成")
    assert.match(await trace.locator(".ui-trw__task").textContent(), /让我们在瑞典上线/)

    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["tool call card", "turn timeline", "merged lines with chips", "tense follows state", "live spinner on the running call", "plan card", "thinking stream and fold", "end check", "1280/390px", "light/dark"] }))
} finally { await browser.close() }
