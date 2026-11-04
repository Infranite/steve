import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
/* ST-46: tables open in the page's own scroll; A2UI interfaces draw, filter locally, submit once, survive a reload,
 * recover from a failed submit and degrade when the description is wrong or still being written. */
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const url = base + "/test/fixtures/a2ui.html"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const errors = [], external = []
const hub = (page) => page.evaluate(() => window.fixtureHub.load())
const setHub = (page, f) => page.evaluate((src) => { const h = window.fixtureHub; const s = h.load(); new Function("s", src)(s); h.save(s) }, f)
const shot = async (page, name, sel) => {
  if (!shots) return
  if (sel) await page.locator(sel).first().evaluate((el) => el.scrollIntoView({ block: "start" }))
  await page.waitForTimeout(250)
  await page.screenshot({ path: `${shots}/${name}.png`, animations: "disabled" })
}
try {
  for (const width of [1440, 390]) for (const theme of ["light", "dark"]) {
    const tag = `${width}-${theme}`
    const page = await browser.newPage({ viewport: { width, height: width === 390 ? 844 : 1000 }, reducedMotion: "reduce", colorScheme: theme })
    page.on("pageerror", (e) => errors.push(`${tag}: ${e.message}`))
    await page.route("**/*", (r) => { const u = new URL(r.request().url()); if (u.origin !== base) { external.push(u.href); return r.abort() } return r.continue() })
    await page.goto(url + "?reset")
    await page.goto(url) // reloads below keep what this Hub was sent
    await page.locator('[data-msg="table"] [data-md-table]').first().waitFor()
    await page.locator('[data-a2ui-surface="release"]').waitFor()

    /* tables: no frame, no toolbar, no scrolling inside them; only a wide one scrolls, sideways */
    const table = page.locator('[data-msg="table"]')
    assert.equal(await table.locator('[data-streamdown="table-wrapper"], [data-streamdown="table-fullscreen"]').count(), 0)
    const boxes = await table.locator("[data-md-table] > div").evaluateAll((els) => els.map((el) => ({ h: el.scrollHeight - el.clientHeight, w: el.scrollWidth - el.clientWidth, y: getComputedStyle(el).overflowY })))
    assert.equal(boxes.length, 3)
    for (const b of boxes) assert.ok(b.h <= 1 && b.y !== "scroll", `a table scrolls inside: ${JSON.stringify(b)} at ${tag}`)
    if (width === 390) assert.ok(boxes[1].w > 0, "the wide table scrolls sideways on a phone")
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `the page widened at ${tag}`)
    const long = table.locator("[data-md-table]").nth(2)
    assert.equal(await long.locator("tbody tr:visible").count(), 20)
    await long.getByRole("button", { name: "展开其余 20 行" }).click()
    assert.equal(await long.locator("tbody tr:visible").count(), 40)
    await shot(page, `table-${tag}`, '[data-msg="table"]')

    /* the chart filters on the page: 近 7 天 → 近 30 天 */
    const trend = page.locator('[data-a2ui-surface="trend"]')
    await trend.locator('[data-a2ui-chart="line"] svg').waitFor()
    await trend.locator('[data-a2ui-chart="hbar"] [data-hbar]').first().waitFor()
    await trend.locator('[data-a2ui-chart="line"] [data-a2ui-chart-table]').click()
    assert.equal(await trend.locator('[data-a2ui-chart="line"] tbody tr').count(), 7)
    await trend.locator('[data-a2ui-option="30"]').click()
    assert.equal(await trend.locator('[data-a2ui-option="30"]').getAttribute("aria-checked"), "true")
    assert.equal(await trend.locator('[data-a2ui-chart="line"] tbody tr').count(), 30)
    await trend.locator('[data-a2ui-chart="line"] [data-a2ui-chart-table]').click()
    await shot(page, `chart-${tag}`, '[data-msg="ui"]')

    /* a failed check stops the submit before anything is sent */
    const form = page.locator('[data-a2ui-surface="release"]')
    await form.locator('[data-a2ui-button="ok"]').click()
    await form.getByRole("alert").filter({ hasText: "要填版本说明" }).waitFor()
    assert.equal((await hub(page)).calls, 0)

    /* what you typed survives a reload before it is sent */
    await form.getByLabel("版本说明").fill("ST-46 表格与界面")
    await form.getByText("Hub", { exact: true }).click()
    await page.reload()
    await form.waitFor()
    assert.equal(await form.getByLabel("版本说明").inputValue(), "ST-46 表格与界面")

    /* a failed submit says so and can be tried again */
    await setHub(page, "s.fail = true")
    await form.locator('[data-a2ui-button="ok"]').click()
    await page.getByText(/没提交成功：Hub 暂时连不上/).waitFor()
    assert.equal(await form.getAttribute("data-a2ui-state"), "open")
    assert.equal(await form.locator('[data-a2ui-button="ok"]').isEnabled(), true)
    await setHub(page, "s.fail = false; s.calls = 0")

    /* a double click sends once; the surface is then a record, and the action is a line in the conversation */
    await form.locator('[data-a2ui-button="ok"]').dblclick()
    await page.locator('[data-a2ui-surface="release"][data-a2ui-state="submitted"]').waitFor()
    assert.equal((await hub(page)).calls, 1)
    const said = page.locator('[data-a2ui-action="confirm"]')
    await said.waitFor()
    assert.match(await said.innerText(), /提交了「确认发布」[\s\S]*ST-46 表格与界面/)
    assert.equal(await form.locator('[data-a2ui-button="no"]').isDisabled(), true)
    assert.equal(await form.locator('[data-a2ui-button="ok"]').getAttribute("data-a2ui-submitted"), "true")
    const sent = (await hub(page)).thread.find((m) => m.id === "ui").meta.a2ui
    const rec = Object.values(sent)[0]
    assert.deepEqual(rec.context, { name: "ST-46 表格与界面", scope: ["web", "hub"], notify: true })

    /* after a reload: still submitted, showing what was sent */
    await page.reload()
    await page.locator('[data-a2ui-surface="release"][data-a2ui-state="submitted"]').waitFor()
    assert.equal(await form.getByLabel("版本说明").inputValue(), "ST-46 表格与界面")
    assert.equal(await form.getByLabel("版本说明").isDisabled(), true)
    await shot(page, `submitted-${tag}`, '[data-a2ui-surface="release"]')

    /* submitted from another tab meanwhile: this one is told, and shows the record */
    const choose = page.locator('[data-a2ui-surface="pick"]')
    const key = await choose.evaluate((el) => el.closest("[data-a2ui]").getAttribute("data-a2ui"))
    await setHub(page, `s.thread.find((m) => m.id === "choose").meta.a2ui = { "${key}:pick": { name: "choose", label: "就这样", sourceComponentId: "go", context: {}, data: { "$local": { plan: ["b"] } }, at: Date.now(), message: "elsewhere" } }`)
    await choose.locator('[data-a2ui-button="go"]').click()
    await page.getByText("这个界面已经提交过了").waitFor()
    await page.locator('[data-a2ui-surface="pick"][data-a2ui-state="submitted"]').waitFor()
    assert.equal(await choose.locator('[data-a2ui-option="b"] input').isChecked(), true)

    /* old messages, a broken description and an unknown component */
    await page.locator('[data-msg="old"] [data-streamdown="code-block"]').waitFor()
    const bad = page.locator('[data-msg="bad"]')
    await bad.getByText("这条的界面写坏了，文字还在。").waitFor()
    await bad.locator("[data-a2ui-broken]").getByText(/界面描述不是有效的 JSON/).waitFor()
    await bad.getByRole("button", { name: "查看原始描述" }).click()
    await bad.locator('[data-a2ui-broken] [data-streamdown="code-block"]').waitFor()
    await bad.locator("[data-a2ui-unsupported]").getByText("不支持的组件：Fancy3D").waitFor()
    await bad.getByText("其他组件照常显示").waitFor()
    await shot(page, `fallback-${tag}`, '[data-msg="bad"]')
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `the page widened at ${tag}`)

    /* while the block is written: a placeholder, never half a JSON */
    await page.goto(url + "?stream")
    await page.locator("[data-a2ui-pending]").waitFor()
    assert.equal(await page.getByText("createSurface").count(), 0)
    await page.close()
  }
  assert.deepEqual(external, [], "no request leaves the page")
  assert.deepEqual(errors, [])
  console.log("a2ui browser checks passed")
} finally {
  await browser.close()
}
