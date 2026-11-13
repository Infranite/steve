// Vite-only fixture; external Playwright prerequisite, no API calls.
// Session page v3: the conversation's column is centred between the sidebar and the side panel; the panel is one quiet
// column without boxes – a head (harness, model, state, number, item), rows of figures (context with the Context Window
// behind ⓘ, cost, work, age, tokens with the Compact Breakdown behind 明细), outputs, relations, knowledge and a folded
// runtime – from what the Hub reports; harness marks name the harness; on a phone the panel is a sheet.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const errors = []
const shot = async (page, name) => { if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/session-panel-${name}.png`, fullPage: true }) }
async function open(width, q) {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 1000 } })
  page.on("pageerror", (e) => errors.push(e.message))
  await page.route("**/*", (route) => { const u = new URL(route.request().url()); return u.origin !== base || u.pathname.startsWith("/api/") ? route.abort("blockedbyclient") : route.continue() })
  await page.goto(`${base}/test/fixtures/session-panel.html${q}`)
  return page
}
const text = (l) => l.textContent().then((x) => x.replace(/\s+/g, " "))
try {
  for (const [width, dark] of [[1440, false], [1440, true], [1920, false]]) {
    const page = await open(width, dark ? "?dark" : "")
    const panel = page.locator("[data-session-panel]")
    await panel.locator("[data-session-head]").waitFor()
    /* the conversation's column sits in the middle of the room left of the panel, the composer on the same column */
    const room = await panel.evaluate((el) => { const r = el.previousElementSibling.getBoundingClientRect(); return [r.left, r.right] })
    const col = await page.locator("[data-session-runtime]").boundingBox()
    assert.ok(col.width <= 761, `the column keeps its reading width (${col.width})`)
    assert.ok(Math.abs(col.x - room[0] - (room[1] - col.x - col.width)) <= 2, `the column is centred at ${width}: ${col.x - room[0]} | ${room[1] - col.x - col.width}`)
    /* no boxes: nothing in the panel draws a card's lift */
    assert.equal(await panel.locator("[data-session-panel-body] .shadow-\\[var\\(--lift\\)\\], [data-session-panel-body] .bg-card").count(), 0, "the panel has no cards")
    assert.equal(await panel.locator("[data-session-monitor], .ui-agmc").count(), 0, "the turn-by-turn monitor is gone: the transcript has the turns")
    /* the header and the composer's label carry the harness's mark */
    assert.equal(await page.locator("[data-session-mark]").getAttribute("data-session-mark"), "claude-code")
    assert.equal(await page.locator("[data-model] .ui-ccc__model-icon svg").count(), 1, "the composer's label has the harness mark")
    /* the head: harness and model, state, number and item */
    const head = panel.locator("[data-session-head]")
    assert.equal(await head.getAttribute("data-session-head"), "idle")
    assert.match(await text(head), /Claude Code · claude-opus-5-5.*空闲.*会话 #1 ·\s*属于\s*ST-2\s*核实数据契约/)
    /* the figures: context as a bar and its numbers; the estimate and the harness's own bill; work; age; tokens */
    const ctx = panel.locator("[data-session-context]")
    assert.equal(await ctx.getAttribute("data-session-context"), "27")
    assert.match(await text(ctx), /上下文\s*27%\s*54K \/ 200K/)
    assert.match(await text(panel.locator("[data-session-cost]")), /费用\s*\$0\.42\s*harness 报 \$1\.20/)
    assert.match(await text(panel.locator("[data-session-work]")), /工作\s*4 分钟 · 2 回合 · 平均 1 分 45 秒/)
    assert.match(await text(panel.locator("[data-session-time=age]")), /时长\s*7 分钟 · 今天 \d\d:\d\d 开始 · 最近 \d\d:\d\d/)
    assert.match(await text(panel.locator("[data-session-usage]")), /Token\s*945K · 缓存读 95% · 输入 40K · 输出 5K\s*明细/)
    /* ⓘ: the harness's Context Window, its one part and the note on what the harness reports */
    await ctx.locator("[data-ctx-info]").click()
    const ctxPop = page.locator("[data-ctx-pop]")
    await ctxPop.waitFor()
    assert.equal(await ctxPop.locator(".ui-cwc__part").count(), 1)
    assert.match(await text(ctxPop), /54K.*Claude Code 自己上报/)
    await shot(page, `${width}-${dark ? "dark" : "light"}-ctx`)
    await page.keyboard.press("Escape")
    await ctxPop.waitFor({ state: "detached" })
    /* 明细: tokens by kind, the estimate and the cache hit rate */
    await panel.locator("[data-usage-more]").click()
    const use = page.locator("[data-usage-pop]")
    await use.waitFor()
    assert.deepEqual((await use.locator("[data-usage-part]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-usage-part")))).sort(), ["cache_read", "input", "output"])
    assert.match(await text(use.locator("[data-session-cost-detail]")), /估算 \$0\.42 · 缓存命中 96% · Harness 报告 \$1\.20/)
    await page.keyboard.press("Escape")
    await use.waitFor({ state: "detached" })
    /* outputs: only what this session delivered, and the files it changed */
    const outs = panel.locator("[data-session-outputs]")
    assert.equal(await outs.getAttribute("data-session-outputs"), "1")
    assert.match(await text(outs.locator("[data-session-out]")), /数据契约核实摘要\s*报告 · \d\d:\d\d/)
    assert.equal(await outs.locator("[data-session-files]").getAttribute("data-session-files"), "2")
    assert.match(await text(outs.locator("[data-session-files]")), /改动了 2 个文件\s*\+18 −5/)
    /* relations: the item, the collaboration lead upstream, one unit downstream folded to a count, the other session */
    const rel = panel.locator("[data-session-relations]")
    assert.match(await text(rel.locator("[data-rel-group=item]")), /所属\s*ST-2\s*核实数据契约/)
    assert.deepEqual(await rel.locator("[data-rel-group=up] [data-rel-item]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-rel-item"))), ["ST-1"])
    assert.match(await text(rel.locator("[data-rel-group=up]")), /协作的主事项/)
    assert.match(await text(rel.locator("[data-rel-down-toggle]")), /1 个后续事项/)
    assert.equal(await rel.locator("[data-rel-group=down] [data-rel-item]").count(), 0, "downstream is folded")
    await rel.locator("[data-rel-down-toggle]").click()
    assert.deepEqual(await rel.locator("[data-rel-group=down] [data-rel-item]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-rel-item"))), ["ST-3"])
    assert.deepEqual(await rel.locator("[data-rel-group=same] [data-rel-session]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-rel-session"))), ["s0"])
    assert.equal(await rel.locator("[data-session-graph]").count(), 1)
    /* knowledge: its document once, with the number of snippets; why on hover */
    const kn = panel.locator("[data-session-knowledge]")
    assert.equal(await kn.getAttribute("data-session-knowledge"), "1")
    assert.match(await text(kn), /用到的知识\s*1.*设备页方案.*2 段/)
    assert.match(await kn.locator("[data-use]").getAttribute("title"), /协作区 数据契约/)
    /* runtime: folded; open, its rows and the way to resume */
    const rt = panel.locator("[data-session-runtime-info]")
    assert.equal(await rt.locator("[data-session-resume]").count(), 0, "runtime is folded")
    await rt.locator("[data-runtime-toggle]").click()
    assert.match(await text(rt), /Bot\s*开发.*Harness\s*Claude Code.*模型\s*claude-opus-5-5.*设备\s*Hub.*来源\s*Steve.*目录\s*\/tmp\/st-2.*在 harness 中恢复/)
    /* a finished turn's line in the transcript carries its tokens and cost */
    assert.deepEqual(await page.locator("[data-work-usage]").allTextContents(), ["633K token · $0.30", "312K token · $0.12"])
    await shot(page, `${width}-${dark ? "dark" : "light"}`)
    await page.close()
  }
  /* an imported Codex transcript: the OpenAI mark, nothing reported, no item */
  {
    const page = await open(1440, "?codex")
    const panel = page.locator("[data-session-panel]")
    await panel.locator("[data-session-head]").waitFor()
    assert.equal(await page.locator("[data-session-mark]").getAttribute("data-session-mark"), "codex")
    assert.equal(await page.locator("[data-runtime-harness] svg").count(), 1, "the read-only runtime line names the harness with its mark")
    for (const row of ["context", "cost", "usage"]) assert.equal(await panel.locator(`[data-session-${row}]`).count(), 0, `nothing reported: no ${row} row`)
    assert.equal(await panel.locator("[data-session-knowledge]").count(), 0, "no knowledge used: no section for it")
    assert.equal(await panel.locator("[data-session-outputs]").getAttribute("data-session-outputs"), "0", "no item, no outputs: only the files it changed")
    assert.equal(await panel.locator("[data-session-files]").getAttribute("data-session-files"), "2")
    assert.equal(await panel.locator("[data-session-unlinked]").count(), 1)
    assert.match(await text(panel.locator("[data-session-time=imported]")), /导入于.*导入的记录没有各回合的时间/)
    await shot(page, "codex")
    await page.close()
  }
  /* a phone: the panel opens as a sheet from the bottom */
  {
    const page = await open(390, "")
    await page.locator("[data-session-info]").click()
    const sheet = page.locator("[data-session-sheet]")
    await sheet.locator("[data-session-head]").waitFor()
    assert.equal(await sheet.getAttribute("data-side"), "bottom")
    assert.ok((await sheet.boundingBox()).width <= 390)
    await shot(page, "phone")
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("session-panel: ok")
} finally { await browser.close() }
