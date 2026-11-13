// Vite-only fixture; external Playwright prerequisite, no API calls.
// Session page v2: the side panel shows the session at work, its context, consumption, time, outputs, relations,
// knowledge and runtime from what the Hub reports; harness marks name the harness; on a phone the panel is a sheet.
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
try {
  for (const dark of [false, true]) {
    const page = await open(1440, dark ? "?dark" : "")
    const panel = page.locator("[data-session-panel]")
    await panel.locator("[data-session-monitor]").waitFor()
    /* the header and the composer's label carry the harness's mark */
    assert.equal(await page.locator("[data-session-mark]").getAttribute("data-session-mark"), "claude-code")
    assert.equal(await page.locator("[data-model] .ui-ccc__model-icon svg").count(), 1, "the composer's label has the harness mark")
    /* the session at work: harness, model, idle, its two turns on the strip and the estimated bill */
    const mon = panel.locator("[data-session-monitor]")
    assert.equal(await mon.getAttribute("data-session-monitor"), "idle")
    assert.match(await mon.textContent(), /Claude Code.*claude-opus-5-5/)
    assert.match(await mon.textContent(), /\$0\.42/)
    assert.equal(await mon.locator(".ui-agmc__seg").count(), 2)
    await mon.locator(".ui-agmc__step-hit").first().click()
    assert.match(await mon.locator(".ui-agmc__info").textContent(), /第 2 回合.*1 次工具调用.*312K token/, "a turn's card says what opened it, its tools and tokens")
    await page.keyboard.press("Escape")
    /* the context: the harness's total, as one part */
    const ctx = panel.locator("[data-session-context]")
    assert.equal(await ctx.getAttribute("data-session-context"), "27")
    assert.match(await ctx.textContent(), /54K/)
    assert.equal(await ctx.locator(".ui-cwc__part").count(), 1)
    /* consumption: tokens by kind, the estimate and the cache hit rate */
    const use = panel.locator("[data-session-usage]")
    assert.deepEqual((await use.locator("[data-usage-part]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-usage-part")))).sort(), ["cache_read", "input", "output"])
    assert.match(await use.locator("[data-session-cost]").textContent(), /估算 \$0\.42 · 缓存命中 96%/)
    /* time: two turns, worked 3.5 minutes of a 6.7-minute session */
    assert.match(await panel.locator("[data-session-time]").textContent(), /回合2平均 1 分 45 秒/)
    /* outputs: only what this session delivered, and the files it changed */
    const outs = panel.locator("[data-session-outputs]")
    assert.equal(await outs.getAttribute("data-session-outputs"), "1")
    assert.match(await outs.locator("[data-session-out]").textContent(), /数据契约核实摘要/)
    assert.equal(await outs.locator("[data-session-files]").getAttribute("data-session-files"), "2")
    assert.match(await outs.locator("[data-session-files]").textContent(), /\+18 −5/)
    /* relations: the collaboration lead upstream, its item, what follows, the item's other session */
    const rel = panel.locator("[data-session-relations]")
    assert.deepEqual(await rel.locator("[data-rel-group=up] [data-rel-item]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-rel-item"))), ["ST-1"])
    assert.equal(await rel.locator("[data-session-item]").getAttribute("data-session-item"), "ST-2")
    assert.deepEqual(await rel.locator("[data-rel-group=down] [data-rel-item]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-rel-item"))), ["ST-3"])
    assert.deepEqual(await rel.locator("[data-rel-group=same] [data-rel-session]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-rel-session"))), ["s0"])
    /* knowledge: one search, its document once with the number of snippets */
    const kn = panel.locator("[data-session-knowledge]")
    assert.equal(await kn.locator("[data-use-group]").count(), 1)
    assert.equal(await kn.locator("[data-use]").count(), 1)
    assert.match(await kn.textContent(), /协作区 数据契约.*设备页方案.*2 段/)
    /* a finished turn's line in the transcript carries its tokens and cost */
    assert.deepEqual(await page.locator("[data-work-usage]").allTextContents(), ["633K token · $0.30", "312K token · $0.12"])
    await shot(page, dark ? "dark" : "light")
    await page.close()
  }
  /* an imported Codex transcript: the OpenAI mark, nothing reported, no item */
  {
    const page = await open(1440, "?codex")
    const panel = page.locator("[data-session-panel]")
    await panel.locator("[data-session-monitor]").waitFor()
    assert.equal(await page.locator("[data-session-mark]").getAttribute("data-session-mark"), "codex")
    assert.equal(await page.locator("[data-runtime-harness] svg").count(), 1, "the read-only runtime line names the harness with its mark")
    assert.equal(await panel.locator("[data-session-context]").getAttribute("data-session-context"), "unreported")
    assert.equal(await panel.locator("[data-session-unlinked]").count(), 1)
    assert.match(await panel.locator("[data-session-time]").textContent(), /导入于/)
    await shot(page, "codex")
    await page.close()
  }
  /* a phone: the panel opens as a sheet from the bottom */
  {
    const page = await open(390, "")
    await page.locator("[data-session-info]").click()
    const sheet = page.locator("[data-session-sheet]")
    await sheet.locator("[data-session-monitor]").waitFor()
    assert.equal(await sheet.getAttribute("data-side"), "bottom")
    assert.ok((await sheet.boundingBox()).width <= 390)
    await shot(page, "phone")
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log("session-panel: ok")
} finally { await browser.close() }
