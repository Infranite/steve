// Vite fixture with mocked API; external Playwright prerequisite.
// ⌘K: the Command Menu finds items and sessions by their words, sessions and messages the Hub's search found, runs a
// row with the keyboard, and Esc clears the search before it closes. 设置 · 操作记录: the audit trail as the Activity
// list — sentences grouped by day, who did it as a filter (you, a Bot on its own, YOLO for you), a folded run that
// opens into its steps, a step that opens into what happened and who let it, search, and loading earlier entries.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5197"
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const now = Date.now(), min = 60_000
const day = new Date(); day.setHours(12, 0, 0, 0)
const today = (h, m = 0) => new Date(day).setHours(h, m)
const entry = (id, ts, actor, action, target, detail = {}) => ({ id, ts, actor, action, target, detail })
const first = [
  entry(10, today(11, 30), "usr_me", "pair.create", "pair_3"),
  entry(9, today(11, 20), "usr_me", "pair.create", "pair_2"),
  entry(8, today(11, 10), "usr_me", "pair.create", "pair_1"),
  entry(7, today(10, 5), "steve", "item.create", "ST-7", { bot: "dev", source: ["对话", "chat"] }),
  entry(6, today(9, 0), "yolo", "decision.resolve", "dec_1", { choice: "yes", text: "" }),
  entry(5, today(8, 0), "steve", "settings.models", "ws", { before: { "models.route": "a" }, after: { "models.route": "b" } }),
  // the Hub's own upkeep, written under the main Bot's id: not a Bot's work
  entry(4, today(7, 0), "steve", "settings.models.migrate", "ws", {}),
  entry(3, today(6, 0), "steve", "steward.link", "ST-9", { source: "seed" }),
]
const older = [entry(1, now - 3 * 86400_000, "usr_me", "bot.create", "dev")]
let assertions = 0
const errors = []
try {
  for (const [width, theme] of [[1280, "dark"], [390, "light"]]) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height: 860 }, reducedMotion: "reduce" })
    page.on("pageerror", (e) => errors.push(e.message))
    const asked = []
    await page.route((u) => u.pathname.startsWith("/api/"), (route) => {
      const u = new URL(route.request().url()), p = u.pathname
      if (p === "/api/sessions/search") { asked.push(["sessions", u.searchParams.get("q")]); return route.fulfill({ json: u.searchParams.get("q") === "跳转逻辑" ? ["ses_b"] : [] }) }
      if (p === "/api/thread/search") { asked.push(["thread", u.searchParams.get("q")]); return route.fulfill({ json: { messages: u.searchParams.get("q") === "跳转逻辑" ? [{ id: "msg_1", author: "me", text: "登录后的跳转逻辑再看看", item_id: null, created: now - 5 * min, meta: {} }] : [] } }) }
      if (p === "/api/audit") return route.fulfill({ json: u.searchParams.get("before") ? { entries: older, names: { dev: "开发" }, more: false } : { entries: first, names: { usr_me: "Lee", steve: "Steve", "ST-7": "改登录页" }, more: true } })
      if (p === "/api/me") return route.fulfill({ json: { user: { id: "usr_me", name: "Lee", email: null, role: "owner" }, version: "x", web_version: "x" } })
      if (p === "/api/settings/models") return route.fulfill({ json: { purposes: [{ key: "route", label: "分流" }] } })
      return route.fulfill({ status: 404, json: { error: "fixture" } })
    })
    const q = theme === "dark" ? "&dark" : ""

    /* ⌘K */
    await page.goto(base + "/test/fixtures/command-audit.html?cmd" + q)
    await page.locator("[data-open]").waitFor()
    await page.keyboard.press("ControlOrMeta+k")
    const input = page.locator("[data-command-input]")
    await input.waitFor()
    assert.ok(await input.evaluate((el) => el === document.activeElement)); assertions++
    // nothing typed: the actions and pages lead, then the items
    assert.deepEqual(await page.locator("[data-command-group]").evaluateAll((xs) => xs.map((x) => x.dataset.commandGroup)).then((g) => g.slice(0, 3)), ["actions", "pages", "items"]); assertions++
    await input.fill("协作")
    await page.locator("[data-cmd-item='ST-8']").waitFor()
    assert.equal(await page.locator("[data-cmd-item]").count(), 1); assertions++
    // what only the Hub's search found: a session by its transcript, a message of the conversation
    await input.fill("跳转逻辑")
    await page.locator("[data-cmd-session='ses_b']").waitFor()
    await page.locator("[data-cmd-msg='msg_1']").waitFor()
    assert.equal(await page.locator("[data-cmd-session='ses_a']").count(), 0); assertions++
    assert.ok(asked.some(([k, v]) => k === "sessions" && v === "跳转逻辑") && asked.some(([k, v]) => k === "thread" && v === "跳转逻辑")); assertions++
    // found only in its transcript, and says so
    assert.match(await page.locator("[data-cmd-session='ses_b'] .ui-cmc__hint").textContent(), /会话记录里有/); assertions++
    if (shots) await page.screenshot({ path: `${shots}/command-${theme}-${width}.png` })
    // Esc clears the search, the menu stays; nothing matches says so
    await page.keyboard.press("Escape")
    assert.equal(await input.inputValue(), ""); assertions++
    assert.equal(await input.count(), 1); assertions++
    await input.fill("zzqqxx")
    await page.locator("[data-command-empty]").waitFor(); assertions++
    // the keyboard runs a row: ↓ from the first item goes to the second, ↵ opens it
    await input.fill("ST-")
    await page.locator("[data-cmd-item='ST-7']").waitFor()
    await page.keyboard.press("ArrowDown"); await page.keyboard.press("Enter")
    await page.waitForFunction(() => location.hash === "#/items/ST-8")
    assert.equal(await page.locator("[data-command-input]").count(), 0); assertions++
    // items: the number typed first, then by when they last changed
    await page.locator("[data-open]").click(); await input.waitFor()
    await input.fill("ST-9")
    await page.waitForFunction(() => document.querySelectorAll("[data-cmd-item]").length === 1)
    await input.fill("-")
    await page.waitForFunction(() => document.querySelectorAll("[data-cmd-item]").length === 3)
    assert.deepEqual(await page.locator("[data-cmd-item]").evaluateAll((xs) => xs.map((x) => x.dataset.cmdItem)), ["ST-9", "ST-8", "ST-7"]); assertions++
    await input.fill("ST-")
    await page.waitForFunction(() => document.querySelectorAll("[data-cmd-item]").length === 3)
    assert.deepEqual(await page.locator("[data-cmd-item]").evaluateAll((xs) => xs.map((x) => x.dataset.cmdItem)), ["ST-7", "ST-8", "ST-9"]); assertions++
    await input.fill(""); await page.keyboard.press("Escape"); await input.waitFor({ state: "detached" })
    // a session renamed in place while the menu was closed reads by its new name when it opens again
    await page.evaluate(() => window.renameSession("ses_a", "排查登录跳转（新）"))
    await page.locator("[data-open]").click(); await input.waitFor()
    assert.match(await page.locator("[data-cmd-session='ses_a']").innerText(), /排查登录跳转（新）/); assertions++
    // a message found from another page: the conversation that shows next takes the jump
    await input.fill("跳转逻辑")
    await page.locator("[data-cmd-msg='msg_1']").click()
    await page.waitForFunction(() => location.hash === "#/" || location.hash === "")
    await page.waitForTimeout(300)
    const jumped = await page.evaluate(() => window.listenJump())
    assert.equal(jumped?.id, "msg_1"); assertions++
    // Esc on an empty field closes
    await page.locator("[data-open]").click(); await input.waitFor()
    await page.keyboard.press("Escape")
    await input.waitFor({ state: "detached" }); assertions++
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)); assertions++

    /* 设置 · 操作记录 */
    await page.goto(base + "/test/fixtures/command-audit.html?audit" + q)
    await page.locator("[data-activity]").waitFor()
    const rows = page.locator("[data-activity-day] > ul > li")
    assert.deepEqual(await page.locator("[data-activity-day] [data-activity-text]").allInnerTexts(), ["生成配对码", "新建事项「改登录页」", "做出决定", "修改模型设置", "自动选定默认模型", "指定负责人"]); assertions++
    // who did it, said by name and as a filter with counts
    assert.deepEqual(await page.locator("[data-filter]").allInnerTexts().then((xs) => xs.map((x) => x.replace(/\s+/g, " ").trim())), ["全部 8", "你 3", "Bot 自己做的 2", "YOLO 替你 1", "Hub 自动 2"]); assertions++
    assert.match(await rows.nth(1).locator("[data-activity-meta]").innerText(), /Steve/); assertions++
    assert.match(await rows.nth(2).locator("[data-activity-meta]").innerText(), /YOLO 替你/); assertions++
    await page.locator("[data-filter='yolo']").click()
    assert.deepEqual(await page.locator("[data-activity-text]").allInnerTexts(), ["做出决定"]); assertions++
    await page.locator("[data-filter='hub']").click()
    assert.deepEqual(await page.locator("[data-activity-text]").allInnerTexts(), ["自动选定默认模型", "指定负责人"]); assertions++
    await page.locator("[data-filter='all']").click()
    // the folded run: ×3, opening into its three steps, each saying what it was done to
    assert.equal(await rows.nth(0).locator("[data-activity-fold]").innerText(), "×3"); assertions++
    await rows.nth(0).locator(".ui-aga-row__main").first().click()
    assert.equal(await page.locator("[data-activity-steps] > li").count(), 3); assertions++
    assert.deepEqual(await page.locator("[data-activity-steps] [data-activity-task]").allInnerTexts(), ["· pair_3", "· pair_2", "· pair_1"]); assertions++
    await rows.nth(0).locator(".ui-aga-row__main").first().click()
    // a step opens into what happened, with a way to its item; a reason only where the trail has one
    await rows.nth(1).locator(".ui-aga-row__main").click()
    const detail = page.locator("[data-activity-detail]")
    await detail.waitFor()
    assert.match(await detail.innerText(), /item\.create/); assertions++
    assert.match(await detail.innerText(), /操作人\s*Steve/); assertions++
    assert.doesNotMatch(await detail.innerText(), /谁让它做的/); assertions++
    await detail.locator("[data-activity-open]").click()
    await page.waitForFunction(() => location.hash === "#/items/ST-7"); assertions++
    // YOLO's answer says why it could
    await rows.nth(2).locator(".ui-aga-row__main").click()
    assert.match(await page.locator("[data-activity-detail]").innerText(), /谁让它做的[\s\S]*YOLO 开着/); assertions++
    // the Hub's migration is the Hub's, not the Bot's
    await rows.nth(4).locator(".ui-aga-row__main").click()
    const hubDetail = await page.locator("[data-activity-detail]").innerText()
    assert.match(hubDetail, /操作人\s*Hub 自动/); assertions++
    assert.doesNotMatch(hubDetail, /Bot 自己做的|谁让它做的/); assertions++
    assert.match(await rows.nth(4).locator("[data-activity-meta]").innerText(), /Hub 自动/); assertions++
    // a changed setting reads as old → new under the purpose's name
    await rows.nth(3).locator(".ui-aga-row__main").click()
    assert.match(await page.locator("[data-activity-detail]").innerText(), /分流\s*a → b/); assertions++
    if (shots) await page.screenshot({ path: `${shots}/audit-${theme}-${width}.png`, fullPage: true })
    // search, with the keyboard: / focuses it
    await page.locator("body").click({ position: { x: 2, y: 2 } })
    await page.keyboard.press("/")
    assert.ok(await page.locator("[data-activity-search]").evaluate((el) => el === document.activeElement)); assertions++
    await page.keyboard.type("配对")
    assert.deepEqual(await page.locator("[data-activity-text]").allInnerTexts(), ["生成配对码"]); assertions++
    await page.keyboard.press("Escape")
    // earlier entries come in under their own day
    await page.locator("[data-audit-more]").click()
    await page.getByText("新建 Bot「开发」").waitFor()
    assert.equal(await page.locator("[data-audit-more]").count(), 0); assertions++
    assert.equal(await page.locator("[data-activity-day]").count(), 2); assertions++
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)); assertions++
    await page.close()
  }
  assert.deepEqual(errors, []); assertions++
} finally {
  await browser.close()
}
console.log(`command-audit: ${assertions} assertions passed`)
