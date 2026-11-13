// Automation against a fake Hub: a filter whose branches and edges disagree is warned about on the canvas
// (strong for an edge no branch names, weak for a branch without an edge), renaming a branch in the
// inspector renames its edge, the canvas (affectUI Workflow Builder) adds a node from its palette after
// the selected one and plays a dry run's real steps in its run panel, the internal hosts flows may reach
// are your own setting, a new schedule's budget follows its mode until you type one, and Signals says what to do
// when nothing is connected, cuts a long reason to two lines and names a run's steps by their nodes' titles; a flow
// that failed today says so under its description on a phone. 1280 and 390 px,
// light and dark.
import assert from "node:assert/strict"
import { mkdirSync } from "node:fs"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_EXECUTABLE })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5195"
const shots = process.env.STEVE_TEST_SHOTS; if (shots) mkdirSync(shots, { recursive: true })

const graph = {
  nodes: [
    { id: "t", kind: "trigger", title: "手动", config: { source: "manual" } },
    { id: "f", kind: "filter", title: "是不是", config: { rules: [{ label: "是", when: "true" }], else_label: "否" } },
    { id: "a", kind: "action", title: "忽略一", config: { action: "ignore", reason: "一" } },
    { id: "b", kind: "action", title: "忽略二", config: { action: "ignore", reason: "二" } },
  ],
  edges: [{ from: "t", to: "f", label: "" }, { from: "f", to: "a", label: "yes" }, { from: "f", to: "b", label: "否" }],
}
const MODE = { item_auto: "建事项并开始，自动验收", report: "在对话里发一条汇报" }
const flow = { id: "flw_1", kind: "flow", name: "分支对不上", description: "", enabled: false, version: 1, origin: "user", origin_label: "", trigger: { source: "manual", types: [] }, uses_ai: false, outcomes: 2, runs24: 0, last_run: null, created: 1, updated: 1, graph, versions: [{ version: 1, author: "usr_dev", note: "", created: 1 }] }
const dryRun = { id: "run_d", flow_id: "flw_1", flow_version: 1, status: "done", dry_run: true, event_id: null, outcome: "ignored", outcome_label: "忽略了", started: 2, ended: 3, steps: [
  { seq: 1, node_id: "t", status: "ok", branch: null, input: { summary: "样例" }, output: { summary: "样例" }, ms: 1, error: null },
  { seq: 2, node_id: "f", status: "ok", branch: "是", input: {}, output: {}, ms: 2, error: null },
  { seq: 3, node_id: "a", status: "ok", branch: null, input: {}, output: { skipped: "试运行不执行" }, ms: 1, error: null }] }
const run = async ({ width, height, scheme }) => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height }, colorScheme: scheme, reducedMotion: "reduce" })
  page.setDefaultTimeout(10000)
  const errors = []; page.on("pageerror", (e) => errors.push(e.message))
  let hosts = ["nas.local"], sigs = null, sources = []; const created = [], dry = []
  await page.route("**/*", async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith("/api/")) return route.continue()
    const method = route.request().method(), p = u.pathname.slice(4)
    let data = []
    if (p === "/flow-settings/allow-private" && method === "PUT") { hosts = route.request().postDataJSON().hosts.map((h) => h.toLowerCase()).sort(); data = { hosts } }
    else if (p === "/flow-settings/allow-private") data = { hosts }
    else if (p === "/flows" && u.searchParams.get("kind") === "flow") data = [flow]
    else if (p === "/flows/flw_1") data = flow
    else if (p === "/flows/flw_1/runs") data = []
    else if (p === "/flows/flw_1/dry-run") { dry.push(route.request().postDataJSON()); data = dryRun }
    else if (p === "/schedules" && method === "POST") { created.push(route.request().postDataJSON()); data = { id: "flw_s", next_at: null, ...created.at(-1) } }
    else if (p === "/signals") data = sigs ?? { stats: { total: 0, need: 0, item: 0, quiet: 0 }, items: [] }
    else if (p === "/signals/ev_1") data = { ...sigs.items[0], runs: [{ ...dryRun, id: "run_1", dry_run: false }] }
    else if (p === "/sources") data = sources
    else if (p === "/flows/parse-when" || p === "/schedules/parse") data = { cron: "0 9 * * 1", tz: "Asia/Taipei", describe: "每周一 09:00", next: [], by: "rule" }
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(data) })
  })
  const fits = async () => assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `overflow at ${width}px`)
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/${name}-${width}-${scheme}.png`, fullPage: true }) }
  try {
    // the rule page: both mismatches listed, the strong one first
    await page.goto(base + "/test/fixtures/auto.html#/auto/rule/flw_1"); await page.locator("[data-branch-warnings]").waitFor()
    assert.deepEqual(await page.locator("[data-branch-warning]").evaluateAll((xs) => xs.map((x) => x.dataset.branchWarning)), ["strong", "weak"])
    assert.match(await page.locator('[data-branch-warning="strong"]').innerText(), /连线「yes」对不上任何分支/)
    await fits(); await shot("rule-warnings")
    // the warning opens the node; fixing the edge clears both
    await page.locator('[data-branch-warning="strong"] button').click()
    await page.locator('[data-inspector][data-kind="filter"]').waitFor()
    await page.locator('[data-edge-label="a"]').fill("是")
    await page.locator("[data-branch-warnings]").waitFor({ state: "detached" })
    // renaming the else branch renames its edge with it: still no warning
    await page.locator('[data-filter-editor] input').last().fill("其他")
    assert.equal(await page.locator('[data-edge-label="b"]').inputValue(), "其他")
    assert.equal(await page.locator("[data-branch-warnings]").count(), 0)
    await fits(); await shot("rule-renamed")
    // the canvas: a card per node, the filter's two branches as its two outputs
    assert.equal(await page.locator("[data-rule-node]").count(), 4)
    assert.equal(await page.locator('[data-rule-node="f"] [data-out]').count(), 2)
    // a flow that has its nodes opens on the canvas, the palette behind Add node
    assert.equal(await page.locator(".ui-wf-pal").count(), 0)
    // the palette adds a node after the selected one: joined to it, its settings open
    if (width < 640) await page.locator(".ui-wf-scrim").click({ position: { x: 5, y: 5 } })
    await page.locator(".ui-wf-tb__add").click()
    await page.locator('.ui-wf-pal [data-palette-item="action:notify"]').click()
    await page.locator('[data-inspector][data-kind="action"]').waitFor()
    assert.equal(await page.locator("[data-rule-node]").count(), 5)
    await page.locator(".ui-wf-set__head button").last().click(); await page.locator(".ui-wf-set").waitFor({ state: "detached" })
    // dropped on empty canvas with nothing selected, a node joins nothing: its card and the page say so and Save waits
    // (the Hub refuses a node the trigger can't reach); ⌘Z takes the drop back
    const empty = async () => page.evaluate(() => {
      const v = document.querySelector(".ui-wf-vp").getBoundingClientRect()
      for (let y = v.top + 30; y < v.bottom - 90; y += 20) for (let x = v.left + 20; x < v.right - 20; x += 20) {
        const el = document.elementFromPoint(x, y)
        if (el?.classList.contains("ui-wf-vp") && [[-160, -110], [160, 110], [-160, 110], [160, -110], [0, -110], [0, 110], [-160, 0], [160, 0]].every(([dx, dy]) => document.elementFromPoint(x + dx, y + dy)?.classList.contains("ui-wf-vp"))) return { x, y }
      }
      return null
    })
    let spot = await empty(); assert.ok(spot, "an empty spot on the canvas")
    await page.mouse.click(spot.x, spot.y)
    assert.equal(await page.locator(".ui-wf-node[data-selected]").count(), 0)
    if (width < 640 && await page.locator(".ui-wf-scrim").count()) await page.locator(".ui-wf-scrim").click({ position: { x: 5, y: 5 } })
    if (!await page.locator(".ui-wf-pal").count()) await page.locator(".ui-wf-tb__add").click()
    const item = await page.locator('.ui-wf-pal [data-palette-item="filter"]').boundingBox()
    await page.mouse.move(item.x + 20, item.y + item.height / 2); await page.mouse.down()
    await page.mouse.move(item.x + 40, item.y + item.height / 2, { steps: 3 })
    spot = await empty(); assert.ok(spot, "an empty spot while dragging")
    await page.mouse.move(spot.x, spot.y, { steps: 8 }); await page.mouse.up()
    await page.locator("[data-unreachable]").waitFor()
    assert.equal(await page.locator("[data-rule-node]").count(), 6)
    const orphan = await page.locator("[data-unreachable]").getAttribute("data-unreachable")
    assert.match(await page.locator(`[data-rule-node="${orphan}"] .ui-wf-node__chip`).innerText(), /未连接/)
    assert.match(await page.locator("[data-unreachable]").innerText(), /保存/)
    assert.equal(await page.locator("[data-save-rule]").isDisabled(), true)
    await fits(); await shot("rule-orphan")
    if (await page.locator(".ui-wf-set").count()) { await page.locator(".ui-wf-set__head button").last().click(); await page.locator(".ui-wf-set").waitFor({ state: "detached" }) }
    if (width < 640 && await page.locator(".ui-wf-scrim").count()) await page.locator(".ui-wf-scrim").click({ position: { x: 5, y: 5 } })
    await page.locator(".ui-wf-vp").focus(); await page.keyboard.press("ControlOrMeta+z")
    await page.locator("[data-unreachable]").waitFor({ state: "detached" })
    assert.equal(await page.locator("[data-rule-node]").count(), 5)
    assert.equal(await page.locator("[data-save-rule]").isDisabled(), false)
    // Run makes a dry run with the edited graph and plays its steps; the node it didn't pass steps back
    await page.locator(".ui-wf-tb__run").click()
    await page.locator('.ui-wf-run[data-open] [data-step="a"]').waitFor()
    assert.equal(dry.length, 1); assert.equal(dry[0].graph.nodes.length, 5)
    assert.deepEqual(await page.locator(".ui-wf-run [data-step]").evaluateAll((xs) => xs.map((x) => x.dataset.step)), ["t", "f", "a"])
    await page.locator('[data-rule-node="b"][data-skipped]').waitFor()
    await fits(); await shot("rule-dry-run")
    // the flow list: one line per flow, nothing to warn about, no first-run guide once a flow exists
    await page.goto(base + "/test/fixtures/auto.html#/auto/rules"); await page.locator('[data-rule-row="flw_1"]').waitFor()
    assert.equal(await page.locator("[data-auto-attention]").count(), 0)
    assert.equal(await page.locator("[data-auto-guide]").count(), 0)
    // the internal hosts are a setting you save, folded under the list with its value in the line
    const hostsLine = page.locator("[data-allow-private] > button")
    assert.match(await hostsLine.innerText(), /nas\.local/)
    await hostsLine.click(); await page.locator("[data-allow-private] input").waitFor()
    assert.equal(await page.locator("[data-allow-private] input").inputValue(), "nas.local")
    await page.locator("[data-allow-private] input").fill("nas.local, Router.Lan，192.168.1.10")
    await page.locator("[data-allow-private-save]").click()
    await page.waitForFunction(() => document.querySelector("[data-allow-private] input")?.value === "192.168.1.10, nas.local, router.lan")
    await fits(); await shot("allow-private")
    // a new schedule: $20 for an item, $1 for a report, and what you type stays
    await page.goto(base + "/test/fixtures/auto.html#/auto/schedules")
    // no schedules yet: the way to make one sits inside the empty state, not a page-height below it
    await page.locator('[data-slot="empty"] button').waitFor()
    await page.locator("[data-new-schedule]").click()
    const budget = page.locator("#sch-budget"), mode = page.locator('[data-pick="sch-mode"]')
    await budget.waitFor()
    assert.equal(await budget.inputValue(), "20")
    await mode.click(); await page.getByRole("option", { name: MODE.report }).click()
    assert.equal(await budget.inputValue(), "1")
    await budget.fill("3")
    await mode.click(); await page.getByRole("option", { name: MODE.item_auto }).click()
    assert.equal(await budget.inputValue(), "3")
    await fits(); await shot("schedule-budget")
    await page.keyboard.press("Escape")
    // 信号 with nothing connected: what to do about it, and no filters over an empty page
    await page.goto(base + "/test/fixtures/auto.html#/signals"); await page.locator("[data-sig-connect]").waitFor()
    assert.equal(await page.locator("[data-sig-search]").count(), 0)
    await fits(); await shot("signals-connect")
    // a failed signal: its long reason is two lines until you ask for the rest; the group header is just its count
    const reason = "bot step timed out after 120s waiting for the model backend: " + "connect ECONNREFUSED 127.0.0.1:443 ".repeat(12)
    sources = [{ adapter: "imap", source: "mail", name: "邮件", paused: false, triage_bot: null, connections: [{ id: "c1", name: "me", status: "ok", last_sync: 1 }], scopes: [], types: [] }]
    sigs = { stats: { total: 1, need: 0, item: 0, quiet: 0 }, items: [{ id: "ev_1", source: "mail", type: "mail.received", summary: "AWS 账单", group_key: null, group_label: null, thread_key: null, connection: null, actor: null,
      outcome: "failed", outcome_label: "处理失败", reason, handled_by: null, flow: { id: "flw_1", name: "分支对不上", kind: "flow" }, work_item_id: null, decision_id: null, received: Date.now(), raw_available: false }] }
    await page.reload(); await page.locator('[data-sig-group="failed"]').waitFor()
    assert.equal(await page.locator('[data-sig-group="failed"]').innerText(), "处理失败\n1")
    await page.locator('[data-sig-row="ev_1"]').click()
    const box = page.locator("[data-sig-reason] [data-clamp] > span").first()
    await page.locator("[data-sig-reason] [data-clamp-toggle]").waitFor()
    const cut = await box.evaluate((el) => el.clientHeight)
    await page.locator("[data-sig-reason] [data-clamp-toggle]").click()
    assert.ok(await box.evaluate((el) => el.clientHeight) > cut, "the whole reason opens in place")
    await fits(); await shot("signal-reason")
    // 处理过程 names each step by the node's title, not its id
    await page.locator("[data-sig-runs]").click(); await page.locator("[data-step-title]").first().waitFor()
    assert.deepEqual(await page.locator("[data-run-steps] li [data-step-title]").allInnerTexts(), ["手动", "是不是", "忽略一"])
    // the flow failed today: on a phone the count is a line under the description, not a pill beside it
    await page.goto(base + "/test/fixtures/auto.html#/auto/rules"); await page.locator('[data-rule-row="flw_1"]').waitFor()
    assert.equal(await page.locator(width < 640 ? "[data-flow-failed-line]" : "[data-flow-failed]").isVisible(), true)
    assert.equal(await page.locator(width < 640 ? "[data-flow-failed]" : "[data-flow-failed-line]").isVisible(), false)
    await fits(); await shot("flow-failed")
    assert.deepEqual(errors, [])
  } finally { await page.close() }
}
try {
  for (const v of [{ width: 1280, height: 900, scheme: "light" }, { width: 390, height: 844, scheme: "dark" }]) await run(v)
  console.log("auto ok")
} finally { await browser.close() }
