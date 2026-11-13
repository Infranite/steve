/* Run against a local Vite server with STEVE_TEST_WEB; PLAYWRIGHT_MODULE / PLAYWRIGHT_EXECUTABLE are optional.
 * Uses production controls with intercepted APIs. No live workspace or permissions are changed. */
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { settle } from "./settle.mjs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5194"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const errors = []
const longPath = "packages/" + "unbroken-path-segment_".repeat(32) + "/check.ts"
const command = "允许执行：cd workspace && node " + longPath
const status = { on: false, mode: "off", active: false, forced_off: null, forced_reason: null, paused: null, window: null, scopes: [], counts: { decided: 0, declined: 0, scheduled: 0 } }

// Text-only enlargement matters here: the app's typography uses px, not just rem.
async function enlargeText(page, factor, selector = "[data-yolo-dialog]") {
  await page.locator(selector).evaluate((root, factor) => {
    const sizes = [root, ...root.querySelectorAll("*")].map(el => [el, getComputedStyle(el).fontSize, getComputedStyle(el).lineHeight])
    for (const [el, font, line] of sizes) {
      el.style.fontSize = parseFloat(font) * factor + "px"
      if (line !== "normal") el.style.lineHeight = parseFloat(line) * factor + "px"
    }
  }, factor)
}
async function layout(page, selector, footerSelector) {
  const box = page.locator(selector)
  const result = await box.evaluate(el => {
    const bounds = el.getBoundingClientRect()
    const bad = []
    for (const child of [el, ...el.querySelectorAll("*")]) {
      const r = child.getBoundingClientRect(), css = getComputedStyle(child)
      if (!r.width || css.position === "absolute" || css.position === "fixed") continue
      // A checkbox's ::after deliberately extends its hit target; its visible box still must fit.
      if (r.left < bounds.left - 1 || r.right > bounds.right + 1 || (child.clientWidth && child.dataset.slot !== "checkbox" && child.scrollWidth > child.clientWidth + 1)) bad.push(`${child.outerHTML.slice(0, 180)}: ${child.clientWidth}/${child.scrollWidth}, ${r.left}..${r.right}`)
      // Also check rendered text, not only boxes: clipping/ellipsis must not pass as a fix.
      for (const node of child.childNodes) if (node.nodeType === Node.TEXT_NODE && node.textContent.trim()) {
        const range = document.createRange(); range.selectNodeContents(node)
        if ([...range.getClientRects()].some(r => r.left < bounds.left - 1 || r.right > bounds.right + 1)) bad.push("text: " + node.textContent.slice(0, 80))
        if (css.textOverflow === "ellipsis" || css.webkitLineClamp !== "none") bad.push("clipped: " + node.textContent.slice(0, 80))
      }
    }
    return { bad, left: bounds.left, right: bounds.right, top: bounds.top, bottom: bounds.bottom, width: innerWidth, height: innerHeight, scroll: document.documentElement.scrollWidth }
  })
  assert.deepEqual(result.bad, [], `${selector}: no over-wide boxes or clipped text`)
  assert.ok(result.left >= 0 && result.right <= result.width && result.top >= 0 && result.bottom <= result.height, JSON.stringify(result))
  assert.ok(result.scroll <= result.width, "document has no horizontal overflow")
  if (footerSelector) {
    const footer = box.locator(footerSelector)
    assert.ok(await footer.evaluate(el => {
      const r = el.getBoundingClientRect()
      return r.top >= 0 && r.bottom <= innerHeight && [...el.querySelectorAll("button")].every(b => {
        const r = b.getBoundingClientRect()
        const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2)
        return r.left >= 0 && r.right <= innerWidth && r.top >= 0 && r.bottom <= innerHeight && (hit === b || b.contains(hit) || (b.disabled && el.contains(hit)))
      })
    }), "footer buttons are visible and unobstructed before scrolling")
  }
}
async function makePage(width, height, locale = "zh-CN", preview = "long") {
  const page = await browser.newPage({ viewport: { width, height }, locale, reducedMotion: "reduce" })
  page.on("pageerror", e => errors.push(e.message))
  const writes = []
  await page.route(`${base}/api/**`, async route => {
    const req = route.request(), path = new URL(req.url()).pathname, body = req.postDataJSON()
    if (path === "/api/yolo" && body?.preview) {
      if (preview === "error") return route.fulfill({ status: 500, json: { error: "failed/".repeat(70) } })
      const question = preview === "short" ? "允许执行：cd hub-rs/crates/steve-core/src && wc -l changes.rs tests/thread.rs thread/*.rs" : command
      return route.fulfill({ json: { preview: true, would_handle: Array.from({ length: preview === "short" ? 1 : 6 }, (_, i) => ({ decision_id: "d" + i, question, choice: i === 1 ? longPath : "yes", rule: "command", delay_s: i === 2 ? 600 : 0 })), keep_for_you: 0, hard_limited: 0, muted: 0 } })
    }
    if (req.method() !== "GET") writes.push({ path, body })
    if (path === "/api/yolo/actions") return route.fulfill({ json: { windows: [], waiting: 0 } })
    // full access chosen while a turn runs: the harness still asks until the turn ends
    if (path === "/api/yolo/items/ST-100") return route.fulfill({ json: { item_id: "ST-100", level: "full", source: "item", layers: { workspace: "off", project: null, item: "full", forced: null, paused: false },
      sessions: [{ session_id: "s1", readonly: false, status: "after_turn", wanted: "full_access", wanted_from: "full_access", applied: "accept_edits", desired: "full_access", busy: true, refused: null, residual: true }] } })
    if (path === "/api/yolo" || path.startsWith("/api/yolo/scopes/")) return route.fulfill({ json: status })
    throw new Error("Unexpected API request: " + req.url())
  })
  return { page, writes }
}
async function open(page, params = "") {
  await page.goto(base + "/test/fixtures/yolo-layout.html?theme=dark" + params)
  await page.locator("[data-yolo-pill]").click()
  await page.locator("[data-yolo-dialog]").waitFor()
}
async function shot(page, name) {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png`, animations: "disabled" })
}
try {
  // Screenshot dimensions, narrow phones, short landscape, desktop, and English labels.
  for (const [width, height, scale, locale] of [[717, 753, 1, "zh-CN"], [390, 844, 1, "zh-CN"], [320, 568, 1, "zh-CN"], [320, 568, 2, "zh-CN"], [390, 844, 2, "zh-CN"], [717, 753, 2, "zh-CN"], [844, 390, 1, "zh-CN"], [1440, 1000, 1, "en-US"]]) {
    const { page, writes } = await makePage(width, height, locale, width === 717 && scale === 1 ? "short" : "long")
    await open(page)
    await page.locator('[data-yolo-level="full"]').click()
    await page.locator("[data-yolo-would]").waitFor()
    if (scale !== 1) await enlargeText(page, scale)
    const apply = page.locator('[data-yolo-apply="full"]')
    assert.ok(await apply.isDisabled(), "full access still needs explicit acknowledgement")
    // on a phone the dialog slides up as a sheet: check the layout where it rests, not where it passes
    await settle(page)
    await layout(page, "[data-yolo-dialog]", '[data-slot="dialog-footer"]')
    await shot(page, `yolo-${width}-${height}-${scale}x-${locale}-top`)
    const scroll = page.locator("[data-yolo-scroll]")
    // dialogs open with a short scale-in (visual system v2): measure where the footer rests, not where it passes
    await settle(page)
    const footerBefore = await page.locator('[data-slot="dialog-footer"]').boundingBox()
    await page.locator("[data-yolo-include-open]").click()
    await page.locator("[data-yolo-full-ok]").click()
    assert.ok(await apply.isEnabled())
    // the opened section animates its height: scroll once the content has stopped growing
    await scroll.evaluate(async (el) => { let h = -1; while (el.scrollHeight !== h) { h = el.scrollHeight; await new Promise((r) => setTimeout(r, 120)) } el.scrollTop = el.scrollHeight })
    await layout(page, "[data-yolo-dialog]", '[data-slot="dialog-footer"]')
    assert.deepEqual(await page.locator('[data-slot="dialog-footer"]').boundingBox(), footerBefore, "scrolling content does not move the action area")
    // reachable: scrolling brings it into the scroll area (with doubled text on a small phone its label is taller
    // than the area, so the very bottom shows the label's end, not the box at its start)
    const reach = await page.locator("[data-yolo-full-ok]").evaluate(el => {
      el.scrollIntoView({ block: "nearest" })
      const r = el.getBoundingClientRect(), s = el.closest("[data-yolo-scroll]").getBoundingClientRect()
      return { ok: r.top >= s.top && r.bottom <= s.bottom, r: [r.top, r.bottom], s: [s.top, s.bottom] }
    })
    assert.ok(reach.ok, `last acknowledgement is reachable by vertical scrolling (${width}x${height} @${scale}: ${JSON.stringify(reach)})`)
    await shot(page, `yolo-${width}-${height}-${scale}x-${locale}-bottom`)
    await apply.click()
    await page.locator("[data-yolo-dialog]").waitFor({ state: "detached" })
    assert.deepEqual(writes, [{ path: "/api/yolo", body: { mode: "full", include_open: true, confirm: true } }])
    await page.close()
  }
  // Cancellation, changing selection, preview error, forced and paused messages.
  for (const state of ["", "forced", "paused", "error"]) {
    const { page, writes } = await makePage(320, 568, "zh-CN", state === "error" ? "error" : "long")
    await open(page, "&state=" + state)
    if (state === "" || state === "error") {
      await page.locator('[data-yolo-level="full"]').click()
      await page.locator(state === "error" ? "[data-yolo-preview] .text-destructive" : "[data-yolo-would]").waitFor()
      await page.locator("[data-yolo-full-ok]").click()
      await page.locator('[data-yolo-level="on"]').click()
      await page.locator('[data-yolo-level="full"]').click()
      assert.ok(await page.locator('[data-yolo-apply="full"]').isDisabled(), "changing selection resets acknowledgement")
    } else {
      assert.ok(await page.locator('[data-yolo-level="full"]').isDisabled())
      // forced off or paused: Steve handles nothing, so the dial shows 关 and lists nothing as handled for you
      assert.equal(await page.locator('[data-yolo-level="off"]').getAttribute("aria-checked"), "true", `${state}: the dial shows off`)
      assert.equal(await page.locator(".ui-adlc__row:not([data-asks])").count(), 0, `${state}: no action is listed as handled`)
      assert.ok(await page.locator(".ui-adlc__row[data-asks]").count() > 0, `${state}: the actions are listed as asking you`)
      if (state === "paused") {
        // the stored mode (on) is said, not shown as handled; it can still be changed to off
        await page.locator('[data-yolo-stored="on"]').waitFor()
        await page.locator("[data-yolo-stored-off]").click()
        await page.locator('[data-yolo-apply="off"]').waitFor()
      } else assert.equal(await page.locator("[data-yolo-stored]").count(), 0, "nothing stored to mention when the mode is off")
    }
    await settle(page)
    await layout(page, "[data-yolo-dialog]", '[data-slot="dialog-footer"]')
    await page.locator('[data-slot="dialog-footer"] button').first().click()
    await page.locator("[data-yolo-dialog]").waitFor({ state: "detached" })
    assert.deepEqual(writes, [], "cancel never changes settings")
    await page.close()
  }
  // Item and project use the same scope menu and full-access confirmation.
  for (const type of ["item", "project"]) for (const scale of [1, 2]) {
    const { page, writes } = await makePage(320, 568)
    await page.goto(base + "/test/fixtures/yolo-layout.html?theme=dark")
    await page.locator(`[data-yolo-scope="${type}"]`).click()
    await page.locator('[data-yolo-scope-opt="full"]').waitFor()
    if (scale === 2) await enlargeText(page, scale, '[data-slot="dropdown-menu-content"]')
    await layout(page, '[data-slot="dropdown-menu-content"]')
    await shot(page, `yolo-${type}-menu-${scale}x`)
    await page.locator('[data-yolo-scope-opt="full"]').click()
    await page.locator("[data-yolo-scope-confirm]").waitFor()
    if (scale === 2) await enlargeText(page, scale, '[data-yolo-scope-confirm]')
    await settle(page)
    await layout(page, "[data-yolo-scope-confirm]", '[data-slot="alert-dialog-footer"]')
    await shot(page, `yolo-${type}-confirm-${scale}x`)
    // dialogs open with a short scale-in (visual system v2): measure where the footer rests, not where it passes
    await settle(page)
    const footerBefore = await page.locator('[data-slot="alert-dialog-footer"]').boundingBox()
    const header = page.locator('[data-slot="alert-dialog-header"]')
    await header.evaluate(el => { el.scrollTop = el.scrollHeight })
    assert.ok(await header.evaluate(el => {
      const r = el.getBoundingClientRect(), last = el.lastElementChild.getBoundingClientRect()
      return last.bottom <= r.bottom + 1
    }), "the entire risk explanation can be read by scrolling")
    assert.deepEqual(await page.locator('[data-slot="alert-dialog-footer"]').boundingBox(), footerBefore)
    await shot(page, `yolo-${type}-confirm-${scale}x-bottom`)
    await page.locator('[data-slot="alert-dialog-cancel"]').click()
    assert.deepEqual(writes, [])
    await page.locator(`[data-yolo-scope="${type}"]`).click()
    await page.locator('[data-yolo-scope-opt="full"]').click()
    await page.locator("[data-yolo-scope-full-ok]").click()
    for (let n = 0; !writes.length && n < 50; n++) await page.waitForTimeout(100) // the write is sent after the click returns
    // confirming closes the dialog (it used to stay open over the page)
    await page.locator("[data-yolo-scope-confirm]").waitFor({ state: "detached" })
    assert.deepEqual(writes, [{ path: `/api/yolo/scopes/${type}/${type === "item" ? "ST-100" : "p1"}`, body: { mode: "full", confirm: true } }])
    // the executor's line: pending until the turn ends, never shown as in effect, readable on a phone
    await page.locator('[data-yolo-mode="after_turn"] [data-yolo-residual]').waitFor()
    if (scale === 2) await enlargeText(page, scale, "[data-yolo-modes]")
    await layout(page, "[data-yolo-modes]")
    assert.equal(await page.locator('[data-yolo-mode="applied"]').count(), 0)
    await shot(page, `yolo-${type}-modes-${scale}x`)
    await page.close()
  }
  // Answers that come back out of order: a slow earlier load of the same item, or of the item shown before, never
  // replaces the newest one (the Hub's answers are delayed for real, nothing is mocked inside the page).
  {
    const { page } = await makePage(390, 844)
    const line = (status, residual = false) => ({ item_id: "", level: "full", source: "item", layers: { workspace: "off", project: null, item: "full", forced: null, paused: false },
      sessions: [{ session_id: "s1", readonly: false, status, wanted: "full_access", wanted_from: "full_access", applied: status === "applied" ? "full_access" : "accept_edits", desired: null, busy: residual, refused: status === "refused" ? "this node's owner allows at most permission mode auto" : null, residual }] })
    const seen = { "ST-100": 0, "ST-101": 0 }
    await page.route(`${base}/api/yolo/items/*`, async route => {
      const id = new URL(route.request().url()).pathname.split("/").pop()
      const n = seen[id]++
      // ST-100's first answer is slow and stale; its next one is fresh; ST-101 answers at once
      if (id === "ST-100" && n === 0) { await new Promise(r => setTimeout(r, 1200)); return route.fulfill({ json: { ...line("after_turn", true), item_id: id } }) }
      return route.fulfill({ json: { ...line(id === "ST-100" ? "applied" : "refused"), item_id: id } })
    })
    await page.goto(base + "/test/fixtures/yolo-layout.html?theme=dark&swap=1")
    await page.waitForFunction(() => window.__hubEvent)
    await page.evaluate(() => window.__hubEvent({ type: "yolo" }))
    await page.locator('[data-yolo-mode="applied"]').waitFor()
    await page.waitForTimeout(1500)
    assert.equal(seen["ST-100"], 2)
    assert.equal(await page.locator('[data-yolo-mode="after_turn"]').count(), 0, "the late, older answer did not replace the newer one")
    assert.equal(await page.locator('[data-yolo-mode="applied"]').count(), 1)
    // switch items while ST-100's load is on its way: its late answer never shows under ST-101
    seen["ST-100"] = 0
    await page.locator("[data-yolo-swap]").click()
    await page.locator("[data-yolo-swap]").click()
    await page.locator("[data-yolo-swap]").click()
    await page.locator('[data-yolo-mode="refused"]').waitFor()
    await page.waitForTimeout(1500)
    assert.equal(await page.locator("[data-yolo-swap]").textContent(), "ST-101")
    assert.equal(seen["ST-100"], 1, "ST-100 was asked again, slowly, before the switch")
    assert.equal(await page.locator('[data-yolo-mode="after_turn"]').count(), 0, "the item shown before does not overwrite this one")
    assert.equal(await page.locator('[data-yolo-mode="applied"]').count(), 0)
    assert.equal(await page.locator('[data-yolo-mode="refused"]').count(), 1)
    await page.close()
  }
  // ST-26: a session without a bot taken back from full access: pending, with where its mode comes from; an empty
  // wanted from an older Hub never reads as in effect
  for (const [wanted, status, key] of [["ask", "after_turn", "after_turn"], [null, "applied", "unknown"]]) {
    const { page } = await makePage(390, 844)
    await page.route(`${base}/api/yolo/items/*`, route => route.fulfill({ json: { item_id: "ST-100", level: "off", source: "workspace", layers: { workspace: "off", project: null, item: null, forced: null, paused: false },
      sessions: [{ session_id: "s1", readonly: false, status, wanted, wanted_from: "node_default", applied: "full_access", desired: wanted, busy: true, refused: null, residual: false }] } }))
    await page.goto(base + "/test/fixtures/yolo-layout.html?theme=dark")
    await page.locator(`[data-yolo-mode="${key}"]`).waitFor()
    assert.equal(await page.locator('[data-yolo-mode="applied"]').count(), 0, `${status}: not in effect`)
    const text = await page.locator(`[data-yolo-mode="${key}"]`).textContent()
    assert.ok(text.includes("没有 Bot"), text)
    await layout(page, "[data-yolo-modes]")
    await shot(page, `yolo-modes-nobot-${key}`)
    await page.close()
  }
  assert.deepEqual(errors, [], "no browser runtime errors")
  console.log("PASS: YOLO screenshot/phone/landscape/200% text/English layouts, long commands, fixed reachable actions, cancellation and item/project confirmations")
} finally { await browser.close() }
