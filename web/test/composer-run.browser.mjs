// Vite-only fixture; external Playwright prerequisite, no API calls.
// The composers' run controls (affectUI Chat Composer + Run Controls): Send when idle; while a turn runs Stop and Esc,
// and once something is typed 排队 – and in a session 插话, which is what Enter does (⌥ Enter queues). The queue sits
// in the card; the plus menu adds files, an item reference and the chat's commands.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5189"
const shots = process.env.SHOTS
const errors = []
const log = (page) => page.evaluate(() => window.log.splice(0))
try {
  for (const [width, dark] of [[1440, false], [1440, true], [390, false], [390, true]]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 1000 } })
    page.on("pageerror", (e) => errors.push(e.message))
    await page.addInitScript(() => localStorage.setItem("steve-lang", "zh-CN"))
    await page.route("**/*", (route) => new URL(route.request().url()).origin !== base || new URL(route.request().url()).pathname.startsWith("/api/") ? route.abort("blockedbyclient") : route.continue())
    await page.goto(`${base}/test/fixtures/composer-run.html${dark ? "?dark" : ""}`)

    /* a session's running turn: only Stop until something is typed */
    const sess = page.locator('[data-fixture="session"]'), field = sess.locator('[data-composer="session"] textarea')
    await field.waitFor()
    assert.equal(await sess.locator("[data-stop]").count(), 1)
    assert.equal(await sess.locator("[data-send]").count(), 0, "nothing to send yet")
    assert.equal(await sess.locator("[data-model]").innerText(), "Claude Code · claude-opus-5-5")
    assert.equal(await sess.locator("[data-context]").getAttribute("data-context"), "86")
    /* the queue is in the card, above the field, next first */
    assert.deepEqual(await sess.locator("li[data-queued]").evaluateAll((xs) => xs.map((x) => x.getAttribute("data-queued"))), ["q1", "q2"])
    assert.match(await sess.locator("[data-sq-note]").innerText(), /排队 2 条 · 本回合结束后发送/)
    assert.match(await sess.locator('[data-queued="q1"]').innerText(), /下一条/)
    assert.equal(await sess.locator('[data-queued="q2"]').locator("[data-queued-edit], [data-queued-withdraw], [data-queued-send]").count(), 3)
    for (const b of await sess.locator("li[data-queued] button").all()) {
      const r = await b.boundingBox()
      assert.ok(r.x + r.width <= width, "queued row buttons stay inside the viewport")
    }
    if (shots) await page.screenshot({ path: `${shots}/composer-run-${width}-${dark ? "dark" : "light"}.png`, fullPage: true, animations: "disabled" })
    /* 编辑 opens the message in a box as wide as its row (the row's own CSS must not shrink it to its words) */
    await sess.locator('[data-queued="q1"] [data-queued-edit]').click()
    const box = await sess.locator('[data-queued="q1"] [data-queued-input]').boundingBox(), rowBox = await sess.locator('li[data-queued="q1"]').boundingBox()
    assert.ok(box.width >= rowBox.width - 24, `the edit box fills its row: ${box.width} of ${rowBox.width}`)
    if (shots) await page.screenshot({ path: `${shots}/composer-edit-${width}-${dark ? "dark" : "light"}.png`, fullPage: true, animations: "disabled" })
    await sess.locator('[data-queued="q1"] [data-queued-input]').press("Escape")
    assert.equal(await sess.locator("[data-queued-input]").count(), 0)
    assert.deepEqual(await log(page), [], "closing the edit box stops nothing")

    /* typed: Stop goes quiet, 排队 and 插话 appear; Enter steers, ⌥ Enter queues, the button queues */
    await field.fill("先别改测试")
    assert.equal(await sess.locator('[data-send][data-send-mode="steer"]').count(), 1)
    assert.equal(await sess.locator('[data-send-mode="queue"]').count(), 1)
    assert.equal(await sess.locator("[data-stop]").getAttribute("data-quiet"), "true")
    if (shots) await page.screenshot({ path: `${shots}/composer-typed-${width}-${dark ? "dark" : "light"}.png`, fullPage: true, animations: "disabled" })
    await field.press("Enter")
    await field.fill("然后跑 lint")
    await field.press("Alt+Enter")
    await field.fill("最后提交")
    await sess.locator('[data-send-mode="queue"]').click()
    await field.fill("第一行")
    await field.press("Shift+Enter")
    assert.equal(await field.inputValue(), "第一行\n")
    await field.fill("")
    await field.press("Escape")
    assert.deepEqual(await log(page), ["send:steer:先别改测试", "send:queue:然后跑 lint", "send:queue:最后提交", "stop"])
    assert.equal(await field.inputValue(), "", "a sent message leaves the field")
    /* Stop says it is stopping until the turn ends */
    assert.equal(await sess.locator("[data-stop]").isDisabled(), true)
    await sess.locator("[data-toggle-run]").click()
    assert.equal(await sess.locator("[data-stop]").count(), 0)
    assert.equal(await sess.locator("[data-send]").isDisabled(), true, "idle and empty: Send waits for words")
    await field.fill("新的一轮")
    await field.press("Enter")
    assert.deepEqual(await log(page), ["send:idle:新的一轮"])

    /* the conversation while Steve answers: what is typed only queues */
    const chat = page.locator('[data-fixture="chat"]'), chatField = chat.locator('[data-composer="chat"] textarea')
    await chatField.fill("顺便总结一下")
    assert.equal(await chat.locator('[data-send][data-send-queues]').count(), 1)
    assert.equal(await chat.locator('[data-send-mode="steer"]').count(), 0)
    await chatField.press("Enter")
    await chat.locator("[data-stop]").click()
    assert.deepEqual(await log(page), ["chat:queue:顺便总结一下", "chat-stop"])
    /* the plus menu: an item reference goes in at the caret and its menu's trigger is typed; commands run directly */
    await chat.locator("[data-composer-add]").click()
    const menu = chat.locator('.ui-ccc__menu[data-open="true"] [role="menu"]')
    await menu.waitFor()
    assert.ok((await menu.innerText()).includes("新事项"))
    await menu.getByText("新事项", { exact: true }).click()
    assert.deepEqual(await log(page), ["cmd:newItem"])
    await chatField.fill("看看")
    await chat.locator("[data-composer-add]").click()
    await menu.getByText("引用事项", { exact: true }).click()
    assert.equal(await chatField.inputValue(), "看看 #")
    await page.waitForFunction(() => document.activeElement?.closest('[data-composer="chat"]'))

    /* an item's composer: its own toggle after the plus, laid out as a toggle */
    const item = page.locator('[data-fixture="item"]')
    const seg = await item.locator('[data-composer="item"] [data-slot="tabs-list"], [data-composer="item"] [role="tablist"]').first().boundingBox()
    assert.ok(seg && seg.height >= 24 && seg.width > 80, `the toggle keeps its own look: ${JSON.stringify(seg)}`)
    await item.locator('[data-composer="item"] textarea').fill("补一句")
    await item.locator("[data-send]").click()
    assert.deepEqual(await log(page), ["item:补一句"])
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
    await page.close()
  }
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ passed: true, scenarios: ["1440/390 × light/dark", "stop only while empty", "steer on Enter, queue on ⌥ Enter and the button", "Esc stops, stopping state", "idle send", "chat queues", "plus menu commands and # reference", "queue in the card", "queued edit box fills its row", "item toggle"] }))
} finally { await browser.close() }
