// Vite-only fixture for #/sessions: verifies per-session cumulative tokens + USD cost on rows
// and live memory + disk written bytes on active sessions across desktop & mobile, light & dark.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const errors = []

try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 960 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.goto(base + `/test/fixtures/sessions-showcase.html${theme === "dark" ? "?dark" : ""}`)
    const liveRow = page.locator('[data-session-row="s0"]')
    await liveRow.waitFor()
    assert.match((await liveRow.locator("[data-session-cost]").innerText()).replace(/\s+/g, " "), /45K · \$0\.18/)
    assert.match((await liveRow.locator("[data-session-vitals]").innerText()).replace(/\s+/g, " "), /内存 256 MB · 写 40 MB/)

    const waitingRow = page.locator('[data-session-row="s1"]')
    await waitingRow.waitFor()
    assert.match((await waitingRow.locator("[data-session-cost]").innerText()).replace(/\s+/g, " "), /90K · \$0\.36/)
    assert.equal(await waitingRow.locator("[data-session-vitals]").count(), 0, "waiting non-running session has no live vitals")

    // Scroll the first history day group into view so VirtualList mounts ended session s3.
    await page.locator('[data-sessions-group]').nth(1).scrollIntoViewIfNeeded()
    const endedRow = page.locator('[data-session-row="s3"]')
    await endedRow.waitFor()
    assert.match((await endedRow.locator("[data-session-cost]").innerText()).replace(/\s+/g, " "), /180K · \$0\.72/)
    assert.equal(await endedRow.locator("[data-session-vitals]").count(), 0, "ended session has no live vitals")

    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no horizontal overflow at ${width}px ${theme}`)
    await page.close()
  }
  for (const width of [1440, 390]) for (const locale of ["zh-CN", "en"]) {
    const page = await browser.newPage({ locale, viewport: { width, height: 1200 }, reducedMotion: "reduce", colorScheme: "dark" })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.goto(base + `/test/fixtures/sessions-showcase.html?dark&large&sysmono${locale === "en" ? "&en" : ""}`)
    await page.locator('[data-session-row="s0"]').waitFor()
    if (locale === "en") await page.waitForFunction(() => document.documentElement.lang === "en")
    await page.locator('[data-sessions-group]').nth(1).scrollIntoViewIfNeeded()
    await page.locator('[data-session-row="s3"]').waitFor()
    const geom = await page.evaluate((vw) => {
      const singleLine = (el) => {
        const range = document.createRange()
        const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT)
        let lines = 0, textNode
        while ((textNode = walker.nextNode())) {
          if (!textNode.textContent?.trim()) continue
          range.selectNodeContents(textNode)
          const rects = [...range.getClientRects()].filter((r) => r.width > 0 && r.height > 0)
          lines += new Set(rects.map((r) => Math.round(r.top))).size
        }
        return lines <= 1
      }
      const rows = [...document.querySelectorAll("[data-session-row]")]
      const wrappedNo = [], clippedNo = [], wrappedTime = [], clippedTime = [], timeLefts = [], timeRights = [], overflows = []
      for (const r of rows) {
        const rb = r.getBoundingClientRect()
        if (rb.width === 0) continue
        for (const el of r.querySelectorAll("*")) {
          const eb = el.getBoundingClientRect()
          if (eb.width > 0 && eb.right > rb.right + 1) overflows.push(`${r.dataset.sessionRow}: ${el.outerHTML.slice(0, 60)}`)
        }
        const no = r.querySelector("[data-no]")
        if (no) {
          if (!singleLine(no)) wrappedNo.push(r.dataset.sessionRow)
          if (no.scrollWidth > no.clientWidth + 1) clippedNo.push(r.dataset.sessionRow)
        }
        const tm = r.querySelector("[data-session-time]")
        if (tm) {
          if (!singleLine(tm)) wrappedTime.push(r.dataset.sessionRow)
          if (tm.scrollWidth > tm.clientWidth + 1) clippedTime.push(r.dataset.sessionRow)
          const tb = tm.getBoundingClientRect()
          if (tb.width > 0) { timeLefts.push(tb.left); timeRights.push(tb.right) }
        }
      }
      const spread = (xs) => xs.length ? Math.max(...xs) - Math.min(...xs) : 0
      return {
        rowCount: rows.length,
        pageOverflow: document.documentElement.scrollWidth > vw,
        wrappedNo, clippedNo, wrappedTime, clippedTime, overflows,
        timeLeftSpread: spread(timeLefts),
        timeRightSpread: spread(timeRights),
      }
    }, width)
    assert.ok(geom.rowCount >= 4, `expected session rows at ${width}px ${locale}`)
    assert.equal(geom.pageOverflow, false, `no horizontal overflow at ${width}px ${locale}`)
    assert.deepEqual(geom.overflows, [], `no row child overflows at ${width}px ${locale}`)
    assert.deepEqual(geom.wrappedNo, [], `ItemNo stays single-line at ${width}px ${locale}`)
    assert.deepEqual(geom.clippedNo, [], `ItemNo is not clipped at ${width}px ${locale}`)
    assert.deepEqual(geom.wrappedTime, [], `session time stays single-line at ${width}px ${locale}`)
    assert.deepEqual(geom.clippedTime, [], `session time is not clipped at ${width}px ${locale}`)
    if (width >= 640) {
      assert.ok(geom.timeLeftSpread <= 1, `session time left spread <= 1px at ${width}px ${locale}, got ${geom.timeLeftSpread}`)
      assert.ok(geom.timeRightSpread <= 1, `session time right spread <= 1px at ${width}px ${locale}, got ${geom.timeRightSpread}`)
    }
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390px × light/dark", "1440/390px × zh-CN/en × font-large × system-mono", "session rows show tokens + USD cost", "active rows show live memory + disk written bytes", "ItemNo & time column geometry", "no horizontal overflow"] }))
} finally { await browser.close() }
