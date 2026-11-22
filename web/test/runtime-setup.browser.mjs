// Real Web components against an isolated, simulated API. This does not exercise Pi or an actual Node.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:19565"
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const runtime = (id, state, installed = [false, false], extra = {}) => ({
  id, name: id === "fixture-runtime" ? "Pi (API fixture)" : id, ok: state === "ready", version: "", path: "", models: [], note: "Simulated registry data", modes: ["full_access"],
  setup: { state, components: installed.map((on, i) => ({ id: i ? "adapter" : "agent", name: i ? "ACP adapter" : "Agent", package: i ? "@example/acp" : "@example/agent", installed: on, version: on ? "1.0.0" : null, path: on ? "/fixture/managed/" + "component/".repeat(10) + i : null })), can_install: state === "not_installed", reason: "Registry reason: " + state, help: "Registry help: review Node and provider configuration" }, ...extra,
})
const legacy = { id: "legacy", name: "Legacy ACP", ok: true, version: "", path: "", models: [], note: "No setup field", modes: ["ask", "full_access"] }
const bot = (id, harness, perm_mode = "default") => ({ id, name: id, role: "Fixture Bot", main: false, harness, model: null, rules: "", persona: "", proactive: 1, accept_mode: "inherit", perm_mode, avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["fixture"], memories: 0 })
const device = { id: "hub", name: "Fixture Hub", roles: ["Hub"], node_id: "local", connected: true, can_configure: true, revision: 0, os: "fixture", version: "fixture", status: "正常", max_sessions: 4, seen: 0, created: 0 }
const workspace = { id: "fixture", name: "Fixture", description: "", item_prefix: "ST", sort: 0, created: 0 }
const fits = async page => assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
const card = (page, id = "fixture-runtime") => page.locator(`[data-runtime="${id}"]`)
const installs = (page, id = "fixture-runtime") => page.locator(`[data-runtime-install="${id}"]`)
const inventory = page => page.locator("[data-runtime-inventory]")
const fold = page => page.locator("[data-runtime-unavailable-toggle]")
const visibleIds = page => page.locator("[data-runtime]:visible").evaluateAll(es => es.map(e => e.dataset.runtime))
const unfold = async page => {
  const toggle = fold(page)
  if (await toggle.count() && await toggle.getAttribute("aria-expanded") === "false") await toggle.click()
}
const showDetails = async (page, id = "fixture-runtime") => {
  await unfold(page)
  const toggle = card(page, id).locator("[data-runtime-details-toggle]")
  if (await toggle.getAttribute("aria-expanded") === "false") await toggle.click()
}

async function fixture({ runtimes = [legacy, runtime("fixture-runtime", "not_installed")], width = 1280, lang = "zh-CN", dark = false } = {}) {
  const page = await browser.newPage({ locale: lang, viewport: { width, height: 900 }, colorScheme: dark ? "dark" : "light", reducedMotion: "reduce" })
  page.setDefaultTimeout(12000)
  const errors = [], mutations = [], catalogs = []
  const f = { page, errors, mutations, catalogs, runtimes, bots: [bot("dev", "legacy"), bot("pi-bot", "fixture-runtime")], devices: [device], items: [], sessions: [], probeStatus: 200, probes: 0, failInstall: false, rejectInstall: false, restart: false, installed: false, finishAt: Infinity }
  page.on("pageerror", e => errors.push(e.message))
  await page.addInitScript(lang => localStorage.setItem("steve-lang", lang), lang)
  const state = () => ({ version: "fixture", web_version: "fixture", owner: false, workspace, workspaces: [workspace], items: f.items, decisions: [], recent: [], bots: f.bots, allBots: f.bots.map(b => ({ id: b.id, name: b.name, workspaces: b.workspaces })), projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: f.sessions, devices: f.devices, runtimes: f.runtimes })
  await page.route("**/*", async route => {
    const req = route.request(), u = new URL(req.url()), p = u.pathname, method = req.method()
    if (u.origin !== base) return route.abort("blockedbyclient")
    if (p === "/auth/hello") return route.fulfill({ json: { name: "steve", version: "fixture", api: 1, login: { token: false, oidc: false } } })
    if (!p.startsWith("/api/")) return route.continue()
    if (method !== "GET") mutations.push({ method, path: p, body: req.postData() ? req.postDataJSON() : null })
    let data
    if (p === "/api/state") data = state()
    else if (p === "/api/devices") {
      if (u.searchParams.get("fresh") !== "1") return route.fulfill({ json: { devices: f.devices, runtimes: f.runtimes } })
      f.probes++
      if (f.probeStatus !== 200) return route.fulfill({ status: f.probeStatus, json: { error: "fixture: Node probe unreachable" } })
      if (f.probes >= f.finishAt) {
        f.runtimes = f.runtimes.map(r => r.id === "fixture-runtime" ? runtime(r.id, "ready", [true, true]) : r)
        f.finishAt = Infinity
      }
      data = { devices: f.devices, runtimes: f.runtimes }
    } else if (p.endsWith("/install") && method === "POST") {
      assert.equal(p, "/api/system/harnesses/fixture-runtime/install")
      assert.deepEqual(req.postDataJSON(), {}, "installation accepts no arbitrary parameters")
      if (f.failInstall) return route.fulfill({ status: 503, json: { error: "fixture: npm offline" } })
      if (!f.rejectInstall) {
        f.runtimes = f.runtimes.map(r => r.id === "fixture-runtime" ? runtime(r.id, "installing") : r)
        if (!f.restart) f.finishAt = f.probes + 3
      }
      data = { accepted: !f.rejectInstall, restart: f.restart }
    } else if (p === "/api/nodes") data = { nodes: [] }
    else if (p === "/api/vitals") data = { nodes: {} }
    else if (p === "/api/node-tags") data = { tags: {} }
    else if (p === "/api/system/models") {
      const h = u.searchParams.get("harness"); catalogs.push(h)
      data = { harness: h, name: h, source: "fixture", default: "fixture-model", families: ["fixture"], models: [{ id: "fixture-model", label: "fixture-model", family: "fixture", via: null, note: null, efforts: [] }] }
    } else if (p === "/api/settings/models") data = {
      steve: { bot_id: "dev", name: "dev", harness: "legacy", runs_on: "legacy", model: null, models: [], effort: null, note: null }, max: 3,
      purposes: [{ key: "chat", label: "Chat", desc: "Fixture purpose", follows: false, value: { harness: "legacy", models: [], effort: null }, effective: { harness: "legacy", models: [], effort: null }, recent: null, recommended: null }],
      agents: [{ harness: "legacy", name: "Legacy ACP", models: [], default: null, value: null }],
      harnesses: f.runtimes.filter(r => r.ok).map(r => ({ id: r.id, name: r.name, efforts: [], default_models: [] })),
    }
    else if (p === "/api/bots/pi-bot" && method === "PATCH") { const b = f.bots.find(b => b.id === "pi-bot"); Object.assign(b, req.postDataJSON()); data = b }
    else if (p === "/api/bots" && method === "POST") { const b = { ...bot("created-bot", "legacy"), ...req.postDataJSON() }; f.bots.push(b); data = b }
    else if (p === "/api/items" && method === "POST") {
      const body = req.postDataJSON(), b = f.bots.find(b => b.id === body.bot)
      const item = { id: "ST-900", workspace_id: "fixture", no: 900, title: body.goal, status: "run", stage: "执行", bot_id: b.id, project_id: null, cwd: "/fixture", branch: null, plan: [], now: "Fixture: session dispatched", accept_mode: null, priority: "中", archived: false, star: false, due: null, snooze: null, source: ["web", ""], parent: null, created: 0, updated: 0, goal: body.goal, goals: 1, tags: [], deps: [], sessionIds: ["fixture-session"], open: [], queued: 0, live: false }
      f.items = [item]
      f.sessions = [{ id: "fixture-session", workspace_id: "fixture", item_id: item.id, no: 1, bot_id: b.id, harness: b.harness, model: b.model, cwd: "/fixture", status: "starting", used: null, size: null, cost: null, title: body.goal, device_id: "hub", source: "Steve", readonly: false, busy: false, queued: [], created: 0, updated: 0 }]
      data = item
    } else if (p === "/api/items/ST-900") data = { ...f.items[0], goalsList: [{ version: 1, text: f.items[0].goal, created: 0 }], timeline: [], artifacts: [], decisions: [], dependents: [] }
    else if (p.endsWith("/events")) data = []
    else return route.fulfill({ status: 404, json: {} })
    return route.fulfill({ json: data })
  })
  f.open = async (hash = "#/devices/nodes/local") => { await page.goto(base + "/" + hash); await page.locator(hash.startsWith("#/devices") ? "[data-panel=runtimes]" : "[data-slot=sidebar]").first().waitFor() }
  return f
}

try {
  const cases = [
    runtime("fixture-runtime", "not_installed"), runtime("body-only", "not_installed", [true, false]), runtime("adapter-only", "not_installed", [false, true]),
    runtime("unknown", "unknown"), runtime("ready", "ready", [true, true], { version: "2.4.1", path: "/fixture/ready/adapter", auth: [{ id: "api", name: "API credentials" }] }), runtime("probe-failed", "unavailable", [true, true], { error: "fixture: provider credentials missing" }),
    runtime("no-npm", "unavailable", [false, false], { setup: { ...runtime("x", "unavailable").setup, reason: "fixture: Node/npm missing", can_install: false } }),
    runtime("offline", "unavailable", [true, false], { setup: { ...runtime("x", "unavailable", [true, false]).setup, error: "fixture: offline", can_install: true } }),
    runtime("external-install", "installing"), runtime("ready-but-failed", "ready", [true, true], { ok: false }), legacy,
    { ...legacy, id: "custom-command", custom: true, path: "/fixture/custom/adapter" },
    ...Array.from({ length: 5 }, (_, i) => runtime(`extra-${i}`, "not_installed", [false, false], { custom: i === 0 })),
  ]
  for (const [width, lang, dark] of [[1280, "zh-CN", false], [375, "zh-CN", true], [320, "zh-CN", false], [1280, "en", true], [375, "en", false], [320, "en", true]]) {
    const f = await fixture({ runtimes: cases, width, lang, dark }), page = f.page
    try {
      await f.open()
      const available = cases.filter(r => r.ok), attention = cases.filter(r => !r.ok && (r.setup?.state === "installing" || r.setup?.error))
      const unavailable = cases.length - available.length - attention.length
      assert.equal(cases.length, 17)
      assert.deepEqual(await visibleIds(page), [...available, ...attention].map(r => r.id), "usable runtimes precede operation feedback; other entries are folded")
      assert.equal(await fold(page).getAttribute("aria-expanded"), "false")
      assert.match(await fold(page).innerText(), new RegExp(`^${unavailable} `))
      assert.doesNotMatch(await inventory(page).innerText(), /\/fixture\/|@example\/|Registry reason|Registry help|Simulated registry|credentials missing|fixture: offline/)
      assert.match(await card(page, "ready").innerText(), /2\.4\.1/)
      assert.equal(await page.locator("[data-runtime-diagnostics]:visible").count(), 0)
      assert.equal(await card(page, "offline").locator("[data-runtime-install-error]").isVisible(), true)
      await page.locator("[data-panel=runtimes]").scrollIntoViewIfNeeded()
      await fits(page)
      if (shots) await page.locator("[data-panel=runtimes]").screenshot({ path: `${shots}/runtime-default-${width}-${lang}.png` })
      await fold(page).focus()
      await page.keyboard.press("Enter")
      assert.equal(await fold(page).getAttribute("aria-expanded"), "true")
      assert.equal(await fold(page).evaluate(el => el === document.activeElement), true)
      assert.equal((await visibleIds(page)).length, 17)
      assert.doesNotMatch(await inventory(page).innerText(), /\/fixture\/|@example\/|Registry reason|Registry help|credentials missing|fixture: offline/)
      // aria-controls resolves to the exact folded cards; hidden content stays out of the accessible tree.
      assert.equal(await fold(page).evaluate(el => el.getAttribute("aria-controls").split(" ").every(id => !!document.getElementById(id)?.querySelector("[data-runtime]"))), true)
      if (shots) await page.locator("[data-panel=runtimes]").screenshot({ path: `${shots}/runtime-expanded-${width}-${lang}.png` })
      for (const r of cases) {
        if (["body-only", "adapter-only", "probe-failed", "unknown", "ready"].includes(r.id)) {
          const toggle = card(page, r.id).locator("[data-runtime-details-toggle]")
          await toggle.focus()
          await page.keyboard.press("Space")
          assert.equal(await toggle.getAttribute("aria-expanded"), "true")
          assert.equal(await toggle.evaluate(el => el === document.activeElement && !!document.getElementById(el.getAttribute("aria-controls"))), true)
        }
        assert.equal(await card(page, r.id).locator("[data-runtime-state]").getAttribute("data-runtime-state"), r.setup?.state === "ready" && !r.ok ? "unavailable" : r.setup?.state ?? "ready")
        assert.equal(await card(page, r.id).locator("[data-runtime-component]").count(), r.setup?.components.length ?? 0)
        assert.equal(await card(page, r.id).locator("[data-runtime-use]").count(), r.ok ? 1 : 0)
      }
      assert.deepEqual(await card(page, "body-only").locator("[data-component-status]").evaluateAll(es => es.map(e => e.dataset.componentStatus)), ["installed", "missing"])
      assert.deepEqual(await card(page, "adapter-only").locator("[data-component-status]").evaluateAll(es => es.map(e => e.dataset.componentStatus)), ["missing", "installed"])
      assert.equal(await card(page, "no-npm").locator("[data-runtime-install]").count(), 0)
      assert.equal(await installs(page, "external-install").isDisabled(), true)
      assert.match(await card(page, "probe-failed").innerText(), /provider credentials missing/)
      assert.match(await card(page, "unknown").innerText(), /未知|Unknown/)
      await fits(page)
      await installs(page).scrollIntoViewIfNeeded()
      assert.equal(await installs(page).isVisible(), true)
      if (shots) await card(page, "ready").screenshot({ path: `${shots}/runtime-details-${width}-${lang}.png` })
      // Details are independent, and folding the inventory leaves focus on its visible trigger.
      await card(page, "ready").locator("[data-runtime-details-toggle]").click()
      assert.equal(await card(page, "ready").locator("[data-runtime-diagnostics]").isVisible(), false)
      assert.equal(await card(page, "unknown").locator("[data-runtime-diagnostics]").isVisible(), true)
      await fold(page).focus()
      await page.keyboard.press("Space")
      assert.equal(await fold(page).getAttribute("aria-expanded"), "false")
      assert.equal(await fold(page).evaluate(el => el === document.activeElement), true)
      assert.deepEqual(await visibleIds(page), [...available, ...attention].map(r => r.id))
      await page.keyboard.press("Tab")
      assert.equal(await page.evaluate(() => !!document.activeElement.closest("[hidden]")), false, "tab cannot enter a folded card")
      assert.deepEqual(f.mutations, [])
      assert.deepEqual(f.errors, [])
    } finally { await page.close() }
  }
  console.log("PASS: 17 runtimes, usable first + visible pending/failure, exact collapsed count, no raw diagnostics by default; independent details and keyboard focus, five states, partial/legacy/custom installs; zh/en × 1280/375/320, light/dark")

  for (const [width, lang] of [[1280, "zh-CN"], [320, "en"]]) {
    const allMissing = Array.from({ length: 17 }, (_, i) => runtime(`missing-${i}`, "not_installed"))
    const f = await fixture({ runtimes: allMissing, width, lang }), page = f.page
    try {
      await f.open()
      assert.deepEqual(await visibleIds(page), [])
      assert.match(await fold(page).innerText(), /^17 /)
      assert.equal((await page.getByRole("button", { name: /17 / }).count()), 1)
      await unfold(page)
      assert.equal((await visibleIds(page)).length, 17)
      await fold(page).click()
      // An installation reported by a later probe is promoted even while the inventory is collapsed.
      f.runtimes = f.runtimes.map((r, i) => i === 0 ? runtime(r.id, "installing") : r)
      await page.locator("[data-runtime-refresh]").click()
      await card(page, "missing-0").waitFor()
      assert.equal(await installs(page, "missing-0").isDisabled(), true)
      assert.match(await fold(page).innerText(), /^16 /)
      assert.equal(await fold(page).getAttribute("aria-expanded"), "false")
      f.runtimes = f.runtimes.map((r, i) => i === 0 ? runtime(r.id, "ready", [true, true]) : r)
      await page.waitForFunction(() => !document.querySelector("[data-runtime-refresh]")?.disabled)
      await page.locator("[data-runtime-refresh]").click()
      await card(page, "missing-0").locator("[data-runtime-use]").waitFor()
      assert.deepEqual(await visibleIds(page), ["missing-0"])
      await fits(page)
      assert.deepEqual(f.errors, [])
    } finally { await page.close() }
    const empty = await fixture({ runtimes: [], width, lang }), ep = empty.page
    try {
      await empty.open()
      assert.equal(await fold(ep).count(), 0)
      assert.equal(await ep.locator("[data-runtime]").count(), 0)
      assert.match(await ep.locator("[data-panel=runtimes]").innerText(), /没有检测到|No harness found/)
      await fits(ep)
    } finally { await ep.close() }
  }
  console.log("PASS: all 17 unavailable → single row, empty inventory, external installation and ready probes update collapsed count without opening it; desktop Chinese and 320px English")

  // Failure, duplicate clicks, install polling, real refresh calls, and the Bot → item → session path.
  const f = await fixture({ runtimes: [legacy, runtime("fixture-runtime", "not_installed"), runtime("spare", "not_installed")] }), page = f.page
  try {
    await f.open()
    await unfold(page)
    f.failInstall = true
    await installs(page).click()
    await card(page).locator("[data-runtime-install-error]").waitFor()
    assert.doesNotMatch(await card(page).innerText(), /npm offline/)
    await fold(page).click()
    assert.equal(await card(page).isVisible(), true, "failed installation remains visible outside the folded inventory")
    await card(page).locator("[data-runtime-details-toggle]").click()
    assert.match(await card(page).innerText(), /npm offline/)
    assert.equal(await card(page).locator("[data-runtime-use]").count(), 0)
    await page.waitForFunction(() => !document.querySelector("[data-runtime-install=fixture-runtime]")?.disabled)
    f.failInstall = false
    await installs(page).evaluate(el => { el.click(); el.click() })
    await card(page).locator('[data-runtime-state="installing"]').waitFor()
    assert.equal(f.mutations.filter(m => m.path.endsWith("/install")).length, 2, "the second click cannot submit twice")
    assert.equal(await installs(page).isDisabled(), true)
    await card(page).locator('[data-runtime-state="ready"]').waitFor()
    assert.equal(await card(page).locator("[data-runtime-details-toggle]").getAttribute("aria-expanded"), "true", "card details survive promotion to usable")
    assert.ok(f.probes >= 4, "completion is read via fresh device probes")
    assert.equal(f.catalogs.includes("fixture-runtime"), false, "unavailable runtime never fetched a model catalog")

    // A successful external command remains selectable/runnable even without verifiable package metadata.
    f.runtimes = f.runtimes.map(r => r.id === "fixture-runtime" ? { ...r, setup: { ...r.setup, can_install: false,
      components: r.setup.components.map(c => ({ ...c, status: "unknown", installed: false })) } } : r)
    await page.locator("[data-runtime-refresh]").click()
    await card(page).locator('[data-component-status="unknown"]').first().waitFor()
    assert.equal(await card(page).locator("[data-component-evidence]").count(), 2)
    assert.equal(await installs(page).count(), 0)

    await page.goto(base + "/#/bots/pi-bot")
    await page.locator('[data-slot=tabs-trigger]:has-text("设置")').click()
    await page.locator('[data-select="bot-harness"]').click()
    await page.getByRole("option", { name: /Legacy ACP/ }).click()
    assert.equal(await page.locator("[data-runtime-permission]").count(), 0)
    await page.locator('[data-select="bot-harness"]').click()
    await page.getByRole("option", { name: /Pi \(API fixture\).*就绪/ }).click()
    await page.locator("[data-runtime-permission]").waitFor()
    assert.match(await page.locator("[data-runtime-permission]").innerText(), /不支持.*每次询问.*完全放行/)
    assert.equal(await page.locator("[data-save-bot]").isDisabled(), false, "existing permission remains editable with its warning")
    await page.locator('[data-model-picker="bot-model"]').click()
    await page.locator('[data-model-opt="fixture-model"]').click()
    await page.locator('[data-choices="bot-perm"] [data-choice=bypassPermissions]').click()
    await page.locator("[data-save-bot]").click()
    await page.waitForFunction(() => !document.querySelector("[data-save-bot]"))
    assert.equal(f.bots.find(b => b.id === "pi-bot").perm_mode, "bypassPermissions")
    assert.equal(f.bots.find(b => b.id === "pi-bot").model, "fixture-model")

    await page.goto(base + "/#/devices/nodes/local")
    await card(page).locator("[data-runtime-use]").click()
    await page.locator("#new-goal").fill("Fixture conversation after runtime installation")
    assert.match(await page.locator('[data-select="new-bot"]').innerText(), /pi-bot/)
    assert.equal(await page.locator('[data-testid="new-go"]').isDisabled(), false)
    await page.locator('[data-testid="new-go"]').click()
    await page.waitForFunction(() => location.hash === "#/items/ST-900")
    const created = f.mutations.find(m => m.path === "/api/items")
    assert.equal(created.body.bot, "pi-bot")
    assert.equal(created.body.mode, "start")
    assert.ok(created.body.client_id)
    assert.equal(f.sessions[0].harness, "fixture-runtime", "simulated backend uses the selected Bot's runtime")
    await page.goto(base + "/#/sessions/fixture-session")
    await page.locator("[data-session-bar]").waitFor()
    assert.match(await page.locator("[data-session-bar]").innerText(), /Fixture conversation/)
    assert.deepEqual(f.errors, [])
  } finally { await page.close() }
  console.log("PASS: failed install → retry, duplicate guard, async installing → ready; ready with unknown component evidence → model/Bot selection + explicit permission → POST /items(mode=start) → simulated session page (not real Pi)")

  // Hub-local availability remains reachable; global configuration does not require a local runtime.
  const g = await fixture({ width: 320, lang: "en" }), p = g.page
  try {
    await g.open()
    await p.goto(base + "/#/bots/pi-bot")
    await p.locator('[data-slot=tabs-trigger]:has-text("Settings")').click()
    await p.locator('[data-select="bot-harness"]').click()
    const option = p.getByRole("option", { name: /Pi \(API fixture\).*Not installed/ })
    assert.notEqual(await option.getAttribute("aria-disabled"), "true")
    await p.keyboard.press("Escape")
    await p.locator('[data-runtime-availability="fixture-runtime"] [data-runtime-setup-link]').click()
    await p.waitForFunction(() => location.hash === "#/devices/nodes/local")
    await p.goto(base + "/#/settings/models")
    await p.locator("[data-models-advanced-toggle]").click()
    await p.locator('[data-purpose="chat"]').click()
    await p.locator('[data-select="harness-chat"]').click()
    assert.equal(await p.getByRole("option", { name: /Pi \(API fixture\).*Not installed/ }).getAttribute("aria-disabled"), "true")
    await p.keyboard.press("Escape")
    await fits(p)
    assert.equal(g.catalogs.includes("fixture-runtime"), false)
    assert.deepEqual(g.errors, [])
  } finally { await p.close() }
  console.log("PASS: unavailable local runtime remains selectable for Bot config; server purpose restriction and local catalog gating retained, 320px English")

  const n = await fixture({ runtimes: [runtime("fixture-runtime", "ready", [true, true])], width: 320 }), np = n.page
  try {
    await n.open()
    await np.goto(base + "/#/bots")
    await np.locator("[data-new-bot]").click()
    await np.locator('[data-choices="new-bot"] [data-choice=blank]').click()
    await np.locator("[data-runtime-permission]").waitFor()
    assert.equal(await np.locator('[data-testid="bot-go"]').isDisabled(), true, "a new Bot does not silently relax its default permission")
    await np.locator('[data-select="new-bot-perm"]').click()
    await np.getByRole("option", { name: /不再询问/ }).click()
    await fits(np)
    await np.locator('[data-testid="bot-go"]').click()
    await np.locator("[data-save-bot]").waitFor()
    const created = n.mutations.find(m => m.path === "/api/bots")
    assert.equal(created.body.harness, "fixture-runtime")
    assert.equal(created.body.perm_mode, "bypassPermissions")
    assert.deepEqual(n.errors, [])
  } finally { await np.close() }
  console.log("PASS: first usable runtime has limited modes; new Bot requires explicit permission and sends it, 320px")

  const h = await fixture(), q = h.page
  try {
    await h.open()
    await unfold(q)
    h.rejectInstall = true
    await installs(q).click()
    await card(q).locator("[data-runtime-install-error]").waitFor()
    assert.match(await card(q).innerText(), /安装未完成/)
    await showDetails(q)
    assert.match(await card(q).innerText(), /未接受安装请求/)
    await q.waitForFunction(() => !document.querySelector("[data-runtime-install=fixture-runtime]")?.disabled)
    h.rejectInstall = false; h.restart = true
    await installs(q).click()
    await card(q).locator("[data-runtime-restart]").waitFor()
    assert.equal(await card(q).locator("[data-runtime-use]").count(), 0)
    h.probeStatus = 503
    await q.locator("[data-runtime-refresh]").click()
    await q.locator("[data-runtime-refresh-error]").waitFor()
    assert.match(await q.locator("[data-runtime-refresh-error]").innerText(), /上次读取结果/)
    assert.doesNotMatch(await q.locator("[data-panel=runtimes]").innerText(), /Node probe unreachable/)
    await q.locator("summary:has([data-runtime-refresh-error])").click()
    await q.locator("[data-runtime-refresh-error-detail]").waitFor()
    assert.match(await q.locator("[data-runtime-refresh-error-detail]").innerText(), /Node probe unreachable/)
    assert.equal(await card(q).locator("[data-runtime-use]").count(), 0)
    h.probeStatus = 200; h.finishAt = h.probes + 1
    await q.locator("[data-runtime-refresh]").click()
    await card(q).locator('[data-runtime-state="ready"]').waitFor()
    await q.locator("[data-runtime-refresh-error]").waitFor({ state: "detached" })
    assert.deepEqual(h.errors, [])
  } finally { await q.close() }
  console.log("PASS: declined installation, restart notice, stale probe error and recovery; no successful availability invented")
} finally { await browser.close() }
