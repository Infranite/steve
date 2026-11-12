// Automation against a fake Hub: a filter whose branches and edges disagree is warned about on the canvas
// (strong for an edge no branch names, weak for a branch without an edge), renaming a branch in the
// inspector renames its edge, the internal hosts flows may reach are your own setting, and a new
// schedule's budget follows its mode until you type one. 1280 and 390 px, light and dark.
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
const run = async ({ width, height, scheme }) => {
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height }, colorScheme: scheme, reducedMotion: "reduce" })
  page.setDefaultTimeout(10000)
  const errors = []; page.on("pageerror", (e) => errors.push(e.message))
  let hosts = ["nas.local"]; const created = []
  await page.route("**/*", async (route) => {
    const u = new URL(route.request().url()); if (u.origin !== base) return route.abort(); if (!u.pathname.startsWith("/api/")) return route.continue()
    const method = route.request().method(), p = u.pathname.slice(4)
    let data = []
    if (p === "/flow-settings/allow-private" && method === "PUT") { hosts = route.request().postDataJSON().hosts.map((h) => h.toLowerCase()).sort(); data = { hosts } }
    else if (p === "/flow-settings/allow-private") data = { hosts }
    else if (p === "/flows" && u.searchParams.get("kind") === "flow") data = [flow]
    else if (p === "/flows/flw_1") data = flow
    else if (p === "/flows/flw_1/runs") data = []
    else if (p === "/schedules" && method === "POST") { created.push(route.request().postDataJSON()); data = { id: "flw_s", next_at: null, ...created.at(-1) } }
    else if (p === "/signals") data = { stats: { total: 0, need: 0, item: 0, quiet: 0 }, items: [] }
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
    // the flow list: the internal hosts are a setting you save
    await page.goto(base + "/test/fixtures/auto.html#/auto/rules"); await page.locator("[data-allow-private] input").waitFor()
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
    assert.deepEqual(errors, [])
  } finally { await page.close() }
}
try {
  for (const v of [{ width: 1280, height: 900, scheme: "light" }, { width: 390, height: 844, scheme: "dark" }]) await run(v)
  console.log("auto ok")
} finally { await browser.close() }
