import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
/* First-run tour: four steps on the Stepper Form, filled from the store (runtimes, projects of this space); the owner ends
 * with the first meeting, anyone else with a first task; skipping is remembered by the Hub for the owner. */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const url = base + "/test/fixtures/onboarding.html"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const errors = [], external = [], hub = []
const open = async (width, theme, q = "") => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: width === 390 ? 844 : 900 }, reducedMotion: "reduce", colorScheme: theme })
  page.on("pageerror", (e) => errors.push(`${width}-${theme}${q}: ${e.message}`))
  await page.route("**/*", (r) => { const u = new URL(r.request().url()); if (u.origin !== base) { external.push(u.href); return r.abort() } if (u.pathname.startsWith("/api/")) { hub.push(u.pathname); return r.abort() } return r.continue() })
  await page.addInitScript(() => localStorage.setItem("steve-lang", "zh-CN"))
  await page.goto(url + q)
  await page.locator("[data-onboard-card]").waitFor()
  return page
}
const card = (page) => page.locator("[data-onboard-card]")
const next = (page) => card(page).locator("button[type=submit]").click()
const step = (page) => card(page).getAttribute("data-onboard")
const heading = (page) => card(page).locator('[data-pane="current"] h2').innerText()
const calls = (page) => page.evaluate(() => window.__calls)
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const tag = `${width}-${theme}`
    const page = await open(width, theme)
    assert.equal(await step(page), "0")
    assert.equal(await card(page).locator(".ui-stpc__step").count(), 4)
    assert.equal(await heading(page), "你好，我是 Steve")
    /* Steve's own buttons inside the form keep their padding (the form's reset only touches its own buttons) */
    const skip = card(page).locator("[data-onboard-skip]")
    assert.ok(parseFloat(await skip.evaluate((el) => getComputedStyle(el).paddingLeft)) > 0, `skip lost its padding at ${tag}`)
    if (shots) await page.screenshot({ path: `${shots}/onboarding-hello-${tag}.png` })

    await next(page)
    await page.waitForFunction(() => document.querySelector("[data-onboard-card]")?.getAttribute("data-onboard") === "1")
    assert.equal(await heading(page), "这台机器上的运行环境")
    const rts = card(page).locator('[data-pane="current"] [data-onboard-runtime]')
    assert.deepEqual(await rts.evaluateAll((els) => els.map((e) => e.getAttribute("data-onboard-runtime"))), ["claude-code", "codex", "gemini"])
    assert.match(await rts.nth(0).innerText(), /3 个模型/)
    assert.match(await rts.nth(2).innerText(), /没有找到 gemini 命令/)
    assert.match(await card(page).locator('[data-pane="current"]').innerText(), /默认用 Claude Code 干活/)
    if (shots) await page.screenshot({ path: `${shots}/onboarding-runtime-${tag}.png` })

    await next(page)
    await page.waitForFunction(() => document.querySelector("[data-onboard-card]")?.getAttribute("data-onboard") === "2")
    const pane = card(page).locator('[data-pane="current"]')
    assert.equal(await pane.locator("[data-onboard-project]").count(), 4, "four of the five live projects; the archived one is not counted")
    assert.match(await pane.innerText(), /有 5 个项目/)
    assert.match(await pane.innerText(), /还有 1 个项目/)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `the page widened at ${tag}`)
    /* a project's path: the home part shortened to ~, set in mono, the folder name kept whole */
    const where = pane.locator('[data-onboard-project="p1"] [title]')
    assert.equal(await where.getAttribute("title"), "~/Agents/clients/a-rather-long-client-folder/steve-dev-loop")
    assert.match(await where.evaluate((el) => getComputedStyle(el).fontFamily), /mono/i)
    assert.ok(await pane.evaluate((el) => el.scrollWidth <= el.clientWidth + 1), `a long path widened the step at ${tag}`)
    /* the bars back to finished steps stay a usable touch target on phones too */
    const back = card(page).locator("button.ui-stpc__step-btn").first()
    assert.ok((await back.boundingBox()).height >= 24, `step button too small at ${tag}`)
    if (shots) await page.screenshot({ path: `${shots}/onboarding-project-${tag}.png` })

    /* Back keeps the tour going from the step before */
    await card(page).locator(".ui-stpc__btn-ghost").click()
    await page.waitForFunction(() => document.querySelector("[data-onboard-card]")?.getAttribute("data-onboard") === "1")
    await next(page); await next(page)
    await page.waitForFunction(() => document.querySelector("[data-onboard-card]")?.getAttribute("data-onboard") === "3")
    assert.equal(await heading(page), "互相认识一下")
    assert.equal(await skip.innerText(), "以后再说")
    if (shots) await page.screenshot({ path: `${shots}/onboarding-meet-${tag}.png` })
    assert.deepEqual(await calls(page), [], "nothing reaches the Hub before the last step")
    await next(page)
    await card(page).waitFor({ state: "detached" })
    assert.deepEqual(await calls(page), ["start"])
    assert.equal(await page.locator("main").getAttribute("data-meeting"), "")
    assert.equal(await page.evaluate(() => localStorage.getItem("steve-onboarded")), "1")
    await page.close()
  }

  /* a link out leaves the tour where it was */
  {
    const page = await open(1440, "light")
    await next(page)
    await card(page).locator('[data-onboard-opt="devices"]').click()
    assert.equal(await page.evaluate(() => location.hash), "#/devices")
    assert.equal(await step(page), "1")
    await page.close()
  }
  /* the owner skips: the Hub remembers it */
  {
    const page = await open(1440, "dark")
    await card(page).locator("[data-onboard-skip]").click()
    await card(page).waitFor({ state: "detached" })
    assert.deepEqual(await calls(page), ["skip"])
    await page.close()
  }
  /* someone else ends with a first task and no meeting */
  {
    const page = await open(390, "light", "?member")
    for (let i = 0; i < 3; i++) await next(page)
    await page.waitForFunction(() => document.querySelector("[data-onboard-card]")?.getAttribute("data-onboard") === "3")
    assert.equal(await heading(page), "交给我第一件事")
    assert.equal(await card(page).locator("button[type=submit]").innerText(), "完成引导")
    await next(page)
    await card(page).waitFor({ state: "detached" })
    assert.deepEqual(await calls(page), [])
    await page.close()
  }
  /* a machine without runtimes or projects: the text says so and no empty list is drawn */
  {
    const page = await open(390, "dark", "?bare")
    await next(page)
    await page.waitForFunction(() => document.querySelector("[data-onboard-card]")?.getAttribute("data-onboard") === "1")
    assert.match(await card(page).locator('[data-pane="current"]').innerText(), /还没有找到可用的 harness/)
    assert.equal(await card(page).locator('[data-pane="current"] ul').count(), 0)
    await next(page)
    await page.waitForFunction(() => document.querySelector("[data-onboard-card]")?.getAttribute("data-onboard") === "2")
    assert.equal(await card(page).locator('[data-pane="current"] ul').count(), 0)
    if (shots) await page.screenshot({ path: `${shots}/onboarding-bare-390-dark.png` })
    await page.close()
  }
  assert.deepEqual(errors, [])
  assert.deepEqual(external, [])
  /* the fixture answers the tour's own calls; only the store's refresh after them reaches for the network */
  assert.deepEqual([...new Set(hub)], ["/api/state"])
  console.log("onboarding: ok")
} finally { await browser.close() }
