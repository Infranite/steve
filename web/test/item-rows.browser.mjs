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
    assert.equal(await row("ST-10").getAttribute("data-item-status"), "pause")
    assert.equal(await row("ST-44").getAttribute("data-item-status"), "run")
    assert.equal(await row("ST-43").getAttribute("data-dense"), "true", "an item closed as failed folds like other closed ones")
    // a real wait: what the card asks and the way to it, not the stale summary
    const r = row("ST-52")
    assert.match(await r.locator('[data-item-wait="approve"]').innerText(), /^审批：合并 PR #12 并打 tag 发版 等 2 件$/)
    assert.equal(await r.locator("[data-item-now]").count(), 0)
    assert.equal((await r.locator("[data-item-wait-go]").innerText()).trim(), "去审批")
    assert.equal((await row("ST-53").locator("[data-item-wait-go]").innerText()).trim(), "去回答")
    // nothing reaches past the row
    for (const id of ["ST-49", "ST-48", "ST-52", "ST-53", "ST-5", "ST-44", "ST-28", "ST-9999", "ST-239", "ST-235", "ST-231", "ST-238"]) {
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
    // the page's 等你 / 进行中 counts are what its filters and its board show
    const sub = await grouped.locator("h1 + div").first().innerText()
    const [, subRun, subWait] = sub.match(/(\d+) 件进行中 · (\d+) 件等你/) ?? []
    assert.ok(subRun !== undefined, "the subtitle says how many run and wait")
    // the board (affectUI Agent Inbox): four columns sorted as the list groups, in the page's width, never sideways
    await grouped.locator("[data-seg=board]:visible").click()
    const board = grouped.locator("[data-items-board]")
    await board.waitFor()
    const fit = await board.evaluate((el) => ({ right: el.getBoundingClientRect().right, scrolls: el.scrollWidth > el.clientWidth + 1 }))
    assert.ok(fit.right <= width + 0.5, `board reaches past the window at ${width}px`)
    assert.equal(fit.scrolls, false, `the board scrolls sideways at ${width}px`)
    assert.equal(await grouped.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, `page scrolls sideways with the board at ${width}px`)
    const lanes = await board.evaluate((el) => [...el.querySelectorAll("[data-board-col]")].map((c) => ({ lane: c.dataset.boardCol, cards: [...c.querySelectorAll("[data-kanban-card]")].map((k) => k.dataset.kanbanCard), box: c.getBoundingClientRect().toJSON() })))
    assert.deepEqual(lanes.map((l) => l.lane), ["needs", "running", "paused", "done"])
    const laneOfCard = (id) => lanes.find((l) => l.cards.includes(id))?.lane
    // a card for you: 等你; working: 进行中; paused item: paused, not waiting on an answer
    for (const id of ["ST-52", "ST-53"]) assert.equal(laneOfCard(id), "needs", `${id} waits on you`)
    for (const id of ["ST-48", "ST-5", "ST-44"]) assert.equal(laneOfCard(id), "running", `${id} is the agents' move`)
    assert.equal(laneOfCard("ST-10"), "paused", "a paused item is paused")
    assert.equal(lanes.find((l) => l.lane === "needs").cards.length, +subWait, "the subtitle's 等你 is the board's 等你 column")
    assert.equal(lanes.find((l) => l.lane === "running").cards.length, +subRun, "the subtitle's 进行中 is the board's 进行中 column")
    // a paused card says why beside its state, not the state twice
    const c10 = board.locator('[data-kanban-card="ST-10"]')
    assert.equal(await c10.locator("[data-state-word]").evaluate((e) => e.firstChild.textContent), "已暂停")
    // every card keeps its state in words, however slim the column
    const words = await board.evaluate((el) => [...el.querySelectorAll("[data-kanban-card]")].map((c) => { const w = c.querySelector("[data-state-word]"); return [c.dataset.kanbanCard, !!w && w.getBoundingClientRect().width > 8 && getComputedStyle(w).display !== "none"] }).filter(([, ok]) => !ok))
    assert.deepEqual(words, [], `a card lost its state word at ${width}px`)
    assert.equal(laneOfCard("ST-30"), "done")
    assert.equal(laneOfCard("ST-43"), undefined, "a failed item is closed and not on the board, as before")
    // side by side on a laptop, one after another on a phone
    const tops = lanes.map((l) => Math.round(l.box.top)), lefts = lanes.map((l) => Math.round(l.box.left))
    if (width === 1066) assert.equal(new Set(tops).size, 1, "three columns side by side at 1066px")
    else assert.equal(new Set(lefts).size, 1, "the columns stack on a phone")
    // a card's words stay inside it
    const over = await board.evaluate((el) => [...el.querySelectorAll("[data-kanban-card]")].flatMap((c) => { const b = c.getBoundingClientRect(); return [...c.querySelectorAll("*")].filter((e) => e.getBoundingClientRect().right > b.right + 1).map((e) => c.dataset.kanbanCard + ": " + e.outerHTML.slice(0, 60)) }))
    assert.deepEqual(over, [], `a card overflows at ${width}px`)
    // the card says what it waits for and how far its plan is; it opens in place, and Open leads to the item
    const c52 = board.locator('[data-kanban-card="ST-52"]')
    assert.match(await c52.innerText(), /审批：|合并 PR #12/)
    assert.equal((await c52.locator("[data-plan]").innerText()).trim(), "2/2")
    await c52.locator(".ui-aic__card-main").click()
    await c52.locator("[data-board-open]").click()
    assert.equal(new URL(grouped.url()).hash, "#/items/ST-52")
    // or in one click from its corner
    await grouped.goBack()
    await board.locator('[data-board-go="ST-48"]').click()
    assert.equal(new URL(grouped.url()).hash, "#/items/ST-48")
    await grouped.goBack()
    // the 等你 filter shows exactly the 等你 column, the 进行中 filter the 进行中 column
    for (const [chip, lane] of [["等你", "needs"], ["进行中", "running"]]) {
      await grouped.locator("[data-items-toolbar]").getByText(chip, { exact: true }).click()
      const shown = await board.evaluate((el) => Object.fromEntries([...el.querySelectorAll("[data-board-col]")].map((c) => [c.dataset.boardCol, [...c.querySelectorAll("[data-kanban-card]")].map((k) => k.dataset.kanbanCard)])))
      assert.deepEqual(shown[lane], lanes.find((l) => l.lane === lane).cards, `the ${chip} filter keeps its column`)
      for (const other of Object.keys(shown)) if (other !== lane) assert.deepEqual(shown[other], [], `the ${chip} filter shows nothing under ${other}`)
    }
    await grouped.close()
  }

  // Geometry & cross-group alignment suite under html.font-large + system-mono across widths & locales
  const layoutCases = [
    { name: "1280-zh", width: 1280, locale: "zh-CN", query: "?groups&large&sysmono", tier: "full" },
    { name: "1066-zh", width: 1066, locale: "zh-CN", query: "?groups&large&sysmono", tier: "full" },
    { name: "720-zh", width: 1066, locale: "zh-CN", query: "?groups&large&sysmono&container=720", tier: "detail" },
    { name: "390-zh", width: 390, locale: "zh-CN", query: "?groups&large&sysmono", tier: "mobile" },
    { name: "1280-en", width: 1280, locale: "en", query: "?groups&large&sysmono&en", tier: "full" },
    { name: "390-en", width: 390, locale: "en", query: "?groups&large&sysmono&en", tier: "mobile" },
  ]
  for (const c of layoutCases) {
    const page = await browser.newPage({ locale: c.locale, viewport: { width: c.width, height: 1800 }, reducedMotion: "reduce", colorScheme: "dark" })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.goto(base + "/test/fixtures/item-rows.html" + c.query)
    await page.locator('[data-items-group="drop"] [data-item-row="ST-238"]').waitFor()
    await page.locator('[data-item-row="ST-9999"]').waitFor()
    if (c.locale === "en") {
      await page.waitForFunction(() => {
        const tm = document.querySelector('[data-item-row="ST-9999"] [data-item-col="time"]')?.textContent ?? ""
        return document.documentElement.lang === "en" && /Yesterday/.test(tm)
      })
    }
    const g = await page.evaluate(({ tier, width }) => {
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
      const spread = (xs) => (xs.length ? Math.max(...xs) - Math.min(...xs) : 0)
      const rows = [...document.querySelectorAll("[data-item-row]")]
      const groups = [...document.querySelectorAll("[data-items-group]")]
      const noLefts = [], noRights = [], titleLefts = [], dotOffsets = [], subLeftDiffs = []
      const colLefts = { kind: [], project: [], plan: [], exec: [], time: [] }
      const colRights = { kind: [], project: [], plan: [], exec: [], time: [] }
      const wrappedNo = [], clippedNo = [], wrappedTime = [], clippedTime = [], badVis = [], execIssues = [], overflows = []

      const isClippedWithinRow = (el, rowRight) => {
        for (let p = el.parentElement; p && p !== el.closest("[data-item-row]"); p = p.parentElement) {
          const ov = getComputedStyle(p).overflowX
          if ((ov === "hidden" || ov === "clip") && p.getBoundingClientRect().right <= rowRight + 1) return true
        }
        return false
      }
      for (const r of rows) {
        const id = r.dataset.itemRow
        const rb = r.getBoundingClientRect()
        for (const el of r.querySelectorAll("*")) {
          const eb = el.getBoundingClientRect()
          if (eb.right > rb.right + 1 && !isClippedWithinRow(el, rb.right)) overflows.push(`${id}: ${el.outerHTML.slice(0, 70)}`)
        }
        const no = r.querySelector("[data-no]")
        if (no) {
          const nb = no.getBoundingClientRect()
          noLefts.push(nb.left)
          noRights.push(nb.right)
          if (!singleLine(no)) wrappedNo.push(id)
          if (no.scrollWidth > no.clientWidth + 1) clippedNo.push(id)
        }
        const title = r.querySelector("[data-item-title]")
        const dot = r.querySelector("[data-row-indicator] > span")
        if (title) {
          const tb = title.getBoundingClientRect()
          titleLefts.push(tb.left)
          if (dot) {
            const db = dot.getBoundingClientRect()
            dotOffsets.push({ id, diff: Math.abs((db.top + db.bottom) / 2 - (tb.top + tb.bottom) / 2) })
          }
          if (!r.hasAttribute("data-dense")) {
            const sub = r.querySelector(":scope > [data-meta], :scope > div > [data-meta]")
            if (sub) subLeftDiffs.push({ id, diff: Math.abs(sub.getBoundingClientRect().left - tb.left) })
          }
        }
        for (const col of ["kind", "project", "plan", "exec", "time"]) {
          const cell = r.querySelector(`[data-item-col="${col}"]`)
          if (!cell) continue
          const vis = getComputedStyle(cell).display !== "none" && cell.getBoundingClientRect().width > 0
          const expectVis =
            col === "time"
              ? true
              : col === "exec"
                ? tier !== "mobile"
                : col === "plan"
                  ? tier !== "mobile"
                  : tier === "full"
          if (vis !== expectVis) badVis.push(`${id}:${col}:vis=${vis}:expect=${expectVis}`)
          if (vis) {
            const cb = cell.getBoundingClientRect()
            colLefts[col].push(cb.left)
            colRights[col].push(cb.right)
            if (col === "time") {
              if (!singleLine(cell)) wrappedTime.push(id)
              if (cell.scrollWidth > cell.clientWidth + 1) clippedTime.push(id)
            }
            if (col === "exec") {
              const ident = cell.querySelector("[data-execution-identity]")
              if (ident && Math.abs(ident.getBoundingClientRect().left - cb.left) > 1) {
                execIssues.push(`${id}:left:${ident.getBoundingClientRect().left - cb.left}`)
              }
              for (const child of cell.querySelectorAll("*")) {
                if (child.getBoundingClientRect().right > cb.right + 1) execIssues.push(`${id}:right:${child.outerHTML.slice(0, 50)}`)
              }
            }
          }
        }
      }

      const r9999 = document.querySelector('[data-item-row="ST-9999"]')
      const side9999 = r9999?.querySelector("[data-item-side]")
      const pri9999 = r9999?.querySelector('[data-item-pri="high"]')
      const due9999 = r9999?.querySelector("[data-item-due]")
      const title9999 = r9999?.querySelector("[data-item-title]")
      const projChipP2 = r9999?.querySelector('[data-project-chip="p2"]')
      return {
        fontLarge: document.documentElement.classList.contains("font-large"),
        themeMono: document.documentElement.dataset.themeMono,
        lang: document.documentElement.lang,
        pageOverflow: document.documentElement.scrollWidth > width,
        rowCount: rows.length,
        groupCount: groups.length,
        overflows,
        wrappedNo,
        clippedNo,
        wrappedTime,
        clippedTime,
        badVis,
        execIssues,
        noLeftSpread: spread(noLefts),
        noRightSpread: spread(noRights),
        titleLeftSpread: spread(titleLefts),
        maxDotDiff: Math.max(0, ...dotOffsets.map((x) => x.diff)),
        maxSubLeftDiff: Math.max(0, ...subLeftDiffs.map((x) => x.diff)),
        colSpreads: Object.fromEntries(Object.keys(colLefts).map((k) => [k, { n: colLefts[k].length, left: spread(colLefts[k]), right: spread(colRights[k]) }])),
        priOk: !!pri9999 && !side9999?.contains(pri9999) && pri9999.getBoundingClientRect().right <= (side9999?.getBoundingClientRect().left ?? 0) + 1,
        dueOk: !!due9999 && !side9999?.contains(due9999) && due9999.getBoundingClientRect().top >= (title9999?.getBoundingClientRect().bottom ?? 0) - 1,
        projTitle: projChipP2?.getAttribute("title") ?? null,
        st9999TimeText: r9999?.querySelector('[data-item-col="time"]')?.textContent ?? "",
      }
    }, { tier: c.tier, width: c.width })

    assert.equal(g.fontLarge, true, `${c.name}: html.font-large set`)
    assert.equal(g.themeMono, "system-mono", `${c.name}: system-mono applied`)
    assert.equal(g.pageOverflow, false, `${c.name}: no page horizontal overflow`)
    assert.ok(g.rowCount >= 14 && g.groupCount >= 5, `${c.name}: multi-group rows rendered`)
    assert.deepEqual(g.overflows, [], `${c.name}: no row child overflows`)
    assert.deepEqual(g.wrappedNo, [], `${c.name}: ItemNo stays single-line`)
    assert.deepEqual(g.clippedNo, [], `${c.name}: ItemNo is not clipped`)
    assert.deepEqual(g.wrappedTime, [], `${c.name}: time column stays single-line`)
    assert.deepEqual(g.clippedTime, [], `${c.name}: time column is not clipped`)
    assert.deepEqual(g.badVis, [], `${c.name}: container column visibility matches tier`)
    assert.deepEqual(g.execIssues, [], `${c.name}: exec slot left-aligned & contained`)
    assert.ok(g.noLeftSpread <= 1 && g.noRightSpread <= 1, `${c.name}: ItemNo aligned <=1px (${g.noLeftSpread}/${g.noRightSpread})`)
    assert.ok(g.titleLeftSpread <= 1, `${c.name}: title left aligned <=1px (${g.titleLeftSpread})`)
    assert.ok(g.maxDotDiff <= 1.5, `${c.name}: status dot centered <=1.5px (${g.maxDotDiff})`)
    assert.ok(g.maxSubLeftDiff <= 1, `${c.name}: sub-lines aligned to title <=1px (${g.maxSubLeftDiff})`)
    for (const [col, s] of Object.entries(g.colSpreads)) {
      if (s.n > 0) {
        assert.ok(s.left <= 1 && s.right <= 1, `${c.name}: col ${col} aligned <=1px (left=${s.left}, right=${s.right})`)
      }
    }
    assert.equal(g.priOk, true, `${c.name}: high priority badge in title area`)
    assert.equal(g.dueOk, true, `${c.name}: due date in open sub-line Meta`)
    if (c.tier === "full") assert.equal(g.projTitle, "long-project-workspace-name", `${c.name}: plain ProjectChip carries full name title`)
    if (c.locale === "en") assert.match(g.st9999TimeText, /^Yesterday \d{2}:\d{2}$/, `${c.name}: English Yesterday time rendered cleanly`)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["new cycle waiting for your word", "running after take-up", "approval reason and entry", "question entry", "no overflow", "board: four columns as the list groups, counts and filters agree, state words at 1066, opens the item", "1066/390px", "dark/light", "font-large + system-mono multi-group geometry (1280/1066/720/390, zh-CN/en)"] }))
} finally { await browser.close() }
