/* Run against the actual Vite app with synthetic Hub responses. No production data.
 * STEVE_PANEL_BASELINE=1 records the old width animation without the regression assertions.
 * Timings are DOM/rAF proxies in headless Chromium, NOT actual presented pixels or WKWebView.
 * Optional STEVE_TEST_SHOTS saves a normal-motion screenshot for visual inspection. */
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { chatPanelState, routeChatPanelHub } from "./fixtures/chat-panel-data.mjs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const baseline = process.env.STEVE_PANEL_BASELINE === "1", results = [], errors = []
const metric = async cdp => Object.fromEntries((await cdp.send("Performance.getMetrics")).metrics.map(m => [m.name, m.value]))
try {
  for (const large of [false, true]) for (const motion of ["no-preference", "reduce"]) for (let run = 0; run < 3; run++) {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, reducedMotion: motion })
    page.on("pageerror", e => errors.push(e.message))
    const calls = await routeChatPanelHub(page, chatPanelState(large))
    await page.addInitScript(() => { localStorage.setItem("steve-chat-rail", "0"); localStorage.setItem("steve-onboarded", "1") })
    await page.goto(base + "/")
    await page.locator(`[data-msg="panel-${large ? 199 : 23}"] .md p`).waitFor()
    await page.evaluate(() => document.fonts.ready)
    // Cold means the panel's first mount after the actual conversation is ready, not Vite/module startup.
    await page.waitForFunction(() => !document.querySelector("[data-chat-scroll]").getAnimations({ subtree: true }).some(a => a.playState === "running" && a.effect.getTiming().iterations !== Infinity))
    const cdp = await page.context().newCDPSession(page)
    await cdp.send("Performance.enable")
    const settled = () => page.waitForFunction(() => ![...document.querySelectorAll("[data-side-panel], [data-side-panel] > aside")].some(el => el.getAnimations().some(a => a.playState === "running")))
    for (const kind of ["history", "rail"]) for (const temperature of ["cold", "warm"]) {
      const before = await metric(cdp), callStart = calls.length
      // Listener runs before React's delegated handler; observation never calls app internals.
      await page.evaluate(kind => {
        window.panelMeasurement = new Promise(resolve => {
          const trigger = document.querySelector(`[data-${kind}-toggle]`)
          trigger.addEventListener("click", () => {
            const start = performance.now(), frames = [], widths = [], mount = { at: null }
            let visible = null, interactive = null, finalWidth = null, complete = null, animated = false, previous = start
            const mo = new MutationObserver(() => { if (mount.at === null && document.querySelector(`[data-${kind}="aside"]`)) mount.at = performance.now() - start })
            mo.observe(document.body, { childList: true, subtree: true })
            function frame(now) {
              const elapsed = performance.now() - start
              frames.push(now - previous); previous = now
              const aside = document.querySelector(`[data-${kind}="aside"]`), scroll = document.querySelector("[data-chat-scroll]")
              widths.push(scroll.getBoundingClientRect().width)
              if (aside) {
                const r = aside.getBoundingClientRect(), parent = aside.parentElement.getBoundingClientRect()
                if (visible === null && parent.width > 1 && r.left < innerWidth && Number(getComputedStyle(aside).opacity) > 0.05) visible = elapsed
                if (finalWidth === null && parent.width >= 319.5) finalWidth = elapsed
                const control = aside.querySelector("button, input"), cr = control?.getBoundingClientRect()
                if (interactive === null && visible !== null && cr && document.elementFromPoint(cr.left + cr.width / 2, cr.top + cr.height / 2)?.closest("button, input") === control && !aside.closest("[inert]")) interactive = elapsed
                const running = [...aside.parentElement.getAnimations(), ...aside.getAnimations()].some(a => a.playState === "running")
                animated ||= running
                if (complete === null && finalWidth !== null && !running && Number(getComputedStyle(aside).opacity) === 1) complete = elapsed
              }
              // Observe a fixed measurement window, not a production cleanup delay.
              if (elapsed < 450 || ((visible === null || complete === null) && elapsed < 2000)) return requestAnimationFrame(frame)
              mo.disconnect()
              resolve({ mountMs: mount.at, visibleFrameMs: visible, interactiveFrameMs: interactive, dockedFrameMs: finalWidth, completeFrameMs: complete, animated, maxFrameMs: Math.max(...frames), widthChanges: widths.slice(1).filter((w, i) => Math.abs(w - widths[i]) > 0.5).length })
            }
            requestAnimationFrame(frame)
          }, { once: true })
        })
      }, kind)
      await page.locator(`[data-${kind}-toggle]`).focus()
      await page.keyboard.press("Enter")
      const measured = await page.evaluate(() => window.panelMeasurement), after = await metric(cdp)
      const row = { size: large ? "large" : "ordinary", motion, run, kind, temperature, ...measured, layouts: after.LayoutCount - before.LayoutCount, layoutMs: (after.LayoutDuration - before.LayoutDuration) * 1000, scriptMs: (after.ScriptDuration - before.ScriptDuration) * 1000, taskMs: (after.TaskDuration - before.TaskDuration) * 1000 }
      results.push(row)
      assert.notEqual(measured.mountMs, null)
      assert.notEqual(measured.visibleFrameMs, null)
      assert.notEqual(measured.interactiveFrameMs, null)
      assert.notEqual(measured.completeFrameMs, null)
      if (!baseline && motion === "no-preference") assert.ok(measured.animated, "normal motion keeps a reveal animation")
      if (!baseline) {
        assert.ok(measured.widthChanges <= 1, JSON.stringify(row))
        // Width is the deterministic regression guard; latency fields are reported separately,
        // not turned into fragile wall-clock assertions on a shared developer machine.
      }
      // Docked, not an overlay; closing after keyboard focus makes the exiting body inert.
      const aside = page.locator(`[data-${kind}="aside"]`)
      const a = await aside.boundingBox(), s = await page.locator("[data-chat-scroll]").boundingBox()
      assert.ok(s.x + s.width <= a.x + 1)
      await aside.locator("button").first().focus()
      await page.locator(`[data-${kind}-close]`).focus()
      await page.keyboard.press("Enter")
      await page.waitForFunction(() => !document.querySelector("[data-side-panel]"))
      if (!baseline) assert.ok(await page.locator(`[data-${kind}-toggle]`).evaluate(el => document.activeElement === el), "keyboard close restores the opener")
      assert.ok(!calls.slice(callStart).some(p => p === "/api/thread/history" || p === "/api/thread/search"), "opening must not wait for history fetch")
    }
    // Switch while animations are running. There must only be one dock allocation,
    // and a canceled exit must never unmount the newly reopened panel.
    if (!baseline) {
      await page.locator("[data-history-toggle]").click()
      await page.locator("[data-rail-toggle]").click()
      await page.locator("[data-history-toggle]").click()
      await page.waitForFunction(() => !!document.querySelector('[data-history="aside"]') && !document.querySelector('[data-rail="aside"]'))
      await settled()
      assert.equal(await page.locator('[data-side-panel]:not([inert])').count(), 1)
      assert.ok(await page.locator("[data-chat-scroll]").evaluate(el => el.scrollHeight - el.scrollTop - el.clientHeight < 3), "bottom remains anchored")
      if (process.env.STEVE_TEST_SHOTS && !large && motion === "no-preference" && run === 0) {
        await mkdir(process.env.STEVE_TEST_SHOTS, { recursive: true })
        await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/chat-panel.png` })
      }
      // Closing history restores the user's previously open Items dock. Close that too
      // before the standalone close/reopen and no-animation cleanup checks.
      await page.locator("[data-history-close]").click()
      await page.locator("[data-rail-close]").click()
      await page.waitForFunction(() => !document.querySelector("[data-side-panel]"))
      await page.locator("[data-history-toggle]").click()
      // Cancel an exit by reopening the same panel, then verify it survives the old
      // animation completion. Drive real DOM buttons, not private panel functions.
      await page.locator("[data-history-close]").click()
      await page.locator("[data-history-toggle]").click()
      await settled()
      await page.locator('[data-history="aside"]').waitFor()
      // Empty animation list (styles disabled/canceled) must clean up without a magic timeout.
      await page.addStyleTag({ content: ".side-panel-body { animation: none !important; }" }).then(async style => {
        await page.locator("[data-history-close]").click()
        await page.waitForFunction(() => !document.querySelector("[data-side-panel]"))
        await style.evaluate(el => el.remove())
      })
      await page.locator("[data-history-toggle]").click()
      if (motion === "no-preference") {
        await page.locator("[data-history-close]").focus()
        await page.keyboard.press("Enter")
        await page.emulateMedia({ reducedMotion: "reduce" })
        await page.waitForFunction(() => !document.querySelector("[data-side-panel]"))
        await page.emulateMedia({ reducedMotion: motion })
        await page.locator("[data-history-toggle]").click()
      }
      await page.locator("[data-history-search]").fill("Synthetic conversation 8.")
      await page.locator('[data-history-hit="panel-8"]').click()
      await page.waitForFunction(() => document.querySelector('[data-msg="panel-8"]')?.hasAttribute("data-flash"))
      await page.locator("[data-jump-latest]").click()
      await page.locator("[data-rail-toggle]").click()
      await page.locator('[data-rail-row="run-ST-2"]').click()
      await page.waitForURL("**/#/items/ST-2")
    }
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ environment: { browser: browser.version(), viewport: "1440x900", app: "Vite dev / real App", fixtures: "24 messages/12 items; 200 messages/400 items", unit: "ms; DOM/rAF proxy, not presented pixels", baseline }, results }, null, 2))
} finally { await browser.close() }
