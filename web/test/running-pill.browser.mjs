// Vite-only fixture; external Playwright prerequisite, API calls answered by the test.
// The running-status pill (affectUI Running Now): in the sidebar on a laptop, in the page header on a phone (and
// nothing there when no item is open); its counts are the items page's, Pause and Resume post the item's action,
// Open and 全部事项 lead to the items.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const errors = []
const open = async (width, theme, query = "") => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 800 }, reducedMotion: "reduce", colorScheme: theme })
  page.on("pageerror", (e) => errors.push(e.message))
  const posts = []
  await page.route((u) => u.pathname.startsWith("/api/"), (route) => {
    const r = route.request()
    if (r.method() === "POST" && /\/api\/items\/[^/]+\/actions$/.test(new URL(r.url()).pathname)) { posts.push([new URL(r.url()).pathname, r.postDataJSON()]); return route.fulfill({ status: 200, json: {} }) }
    return route.fulfill({ status: 503, json: { error: "fixture" } })
  })
  await page.goto(base + "/test/fixtures/running-pill.html" + query)
  await page.locator("h1").waitFor()
  return { page, posts }
}
try {
  for (const theme of ["dark", "light"]) {
    /* a laptop: the pill is a row in the sidebar, and the page header has none */
    const { page, posts } = await open(1440, theme)
    const side = page.locator("[data-sidebar-running] .ui-rnc__trigger")
    await side.waitFor()
    assert.equal(await page.locator(".steve-rn-head:visible").count(), 0, "no second pill in the header beside the sidebar")
    // the same counts as the items page: one in progress, one waiting on you; paused items are under the pill, not in its count
    assert.equal((await side.innerText()).replace(/\s+/g, " ").trim(), "1 进行中 · 1 等你")
    await side.click()
    const pill = page.locator("[data-sidebar-running]"), rows = pill.locator(".ui-rnc__row")
    assert.deepEqual(await rows.evaluateAll((rs) => rs.map((r) => [r.dataset.id, r.dataset.state])), [["ST-53", "needs-you"], ["ST-48", "running"], ["ST-7", "paused"], ["ST-10", "paused"]], "what needs you, then what runs, then what is paused; nothing done")
    // a paused graph is opened, not resumed here, and is announced as paused, not as waiting on you
    const graph = pill.locator('.ui-rnc__row[data-id="ST-10"] .ui-rnc__act button')
    assert.equal(await graph.innerText(), "打开")
    assert.match(await graph.getAttribute("aria-label"), /已暂停$/)
    // Pause and Resume post the item's action
    await pill.locator('.ui-rnc__row[data-id="ST-48"] .ui-rnc__act button').click()
    await pill.locator('.ui-rnc__row[data-id="ST-7"] .ui-rnc__act button').click()
    await page.waitForTimeout(100)
    assert.deepEqual(posts, [["/api/items/ST-48/actions", { action: "pause" }], ["/api/items/ST-7/actions", { action: "resume" }]])
    // Open leads to the item and closes the pill; 全部事项 to the items
    await pill.locator('.ui-rnc__row[data-id="ST-53"] .ui-rnc__act button').click()
    assert.equal(new URL(page.url()).hash, "#/items/ST-53")
    assert.equal(await page.locator(".ui-rnc__panel:visible").count(), 0)
    await side.click()
    await pill.locator(".ui-rnc__all").click()
    assert.equal(new URL(page.url()).hash, "#/items")
    assert.equal(await page.locator(".ui-rnc__panel:visible").count(), 0)
    // the sidebar's 事项 carries no count of its own (the pill says what runs); 待处理 keeps its count of open cards
    assert.equal(await page.locator('[data-nav="items"]').locator("xpath=..").locator('[data-sidebar="menu-badge"]').count(), 0)
    assert.equal((await page.locator('[data-nav="inbox"]').locator("xpath=..").locator('[data-sidebar="menu-badge"]').innerText()).trim(), "1")
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    await page.close()

    /* a phone: the sidebar is away, so the pill sits in the page header; with nothing open, nothing there */
    const phone = (await open(390, theme)).page
    const head = phone.locator(".steve-rn-head .ui-rnc__trigger")
    await head.waitFor()
    assert.ok(await head.isVisible(), "the page header carries the pill on a phone")
    const hb = await head.boundingBox()
    assert.ok(hb.x + hb.width <= 390 + 0.5, "the header pill fits the phone")
    assert.equal(await phone.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    await phone.close()
    const quiet = (await open(390, theme, "?none")).page
    await quiet.waitForTimeout(200)
    assert.equal(await quiet.locator(".steve-rn-head").count(), 0, "no pill in a phone's header when no item is open")
    await quiet.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["sidebar pill on a laptop, none in the header", "counts match the items page", "order and paused-graph Open label", "Pause/Resume post the item action", "Open and all items navigate", "phone header pill; none when nothing is open", "dark/light"] }))
} finally { await browser.close() }
