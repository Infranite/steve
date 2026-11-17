// Running Web + isolated HTTP API fixtures. No Pi installation, Node or model is exercised.
import assert from "node:assert/strict"
import http from "node:http"
import { readFile, mkdir, writeFile } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:19614"
const apiPort = Number(process.env.STEVE_TEST_API_PORT || 19615)
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const ws = { id: "fixture", name: "HTTP fixture", description: "Simulated API", item_prefix: "ST", sort: 0, created: 0 }
const device = { id: "hub", name: "HTTP fixture Hub", roles: ["Hub"], node_id: "local", connected: true, can_configure: true, revision: 0, os: "fixture", version: "fixture", status: "正常", max_sessions: 4, seen: 0, created: 0 }
const runtime = (state, extra = {}) => ({ id: "registry-fixture", name: "Registry fixture", ok: state === "ready", version: "", path: "/fixture/adapter", models: [], note: "HTTP fixture; no real Pi process", modes: ["full_access"], install_hint: "Generic: install components first", ...extra,
  setup: { state, can_install: true, components: ["agent", "adapter"].map(id => ({ id, name: id, package: "@example/" + id, installed: true, status: "installed", path: "/fixture/managed/" + id })), help: "Review provider configuration on the Node; recheck discovery", ...extra.setup } })
const clone = value => JSON.parse(JSON.stringify(value))
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r }); return { promise, resolve } }
const gate = rows => ({ entered: deferred(), release: deferred(), rows })
const entered = async g => {
  let timer
  try { await Promise.race([g.entered.promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("Expected HTTP gate was not reached")), 12000) })]) }
  finally { clearTimeout(timer) }
}
let current
const requests = [], observations = []
const server = http.createServer(async (req, res) => {
  try {
    const f = current, url = new URL(req.url, "http://fixture"), p = url.pathname
    let bytes = ""; for await (const chunk of req) bytes += chunk
    requests.push({ method: req.method, path: req.url, body: bytes ? JSON.parse(bytes) : null })
    let data
    if (p === "/auth/hello") data = { name: "steve", version: "fixture", api: 1, login: { token: false, oidc: false } }
    else if (p === "/api/state") {
      const g = f.stateGates.shift()
      data = { version: "fixture", web_version: "fixture", owner: false, workspace: ws, workspaces: [ws], items: [], decisions: [], recent: [], bots: [], allBots: [], projects: [], tags: [], tagUses: {}, thread: [], settings: { onboard: "1" }, sessions: [], devices: [device], runtimes: clone(g?.rows || f.rows) }
      if (g) { g.entered.resolve(); await g.release.promise }
    } else if (p === "/api/devices") {
      if (url.searchParams.get("fresh") !== "1") { res.writeHead(200, {"content-type":"application/json"}); res.end(JSON.stringify({devices:[device],runtimes:clone(f.rows)})); return }
      f.probes++
      const g = f.deviceGates.shift()
      data = { devices: [device], runtimes: clone(g?.rows || f.rows) }
      if (g) { g.entered.resolve(); await g.release.promise }
    } else if (p.endsWith("/install") && req.method === "POST") {
      const target = f.rows.find(r => p === `/api/system/harnesses/${r.id}/install`)
      assert.ok(target, "installation targets a known registry entry")
      assert.deepEqual(JSON.parse(bytes), {})
      f.installs++
      f.rows = f.rows.map(r => r === target ? runtime("installing", { id: r.id, ok: false, setup: { can_install: false } }) : r)
      data = { accepted: true }
    } else if (p === "/api/nodes") data = { nodes: [] }
    else if (p === "/api/vitals") data = { nodes: {} }
    else if (p === "/api/node-tags") data = { tags: {} }
    else if (p.endsWith("/events")) data = []
    else { res.writeHead(404, { "content-type": "application/json" }); res.end("{}"); return }
    res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify(data))
  } catch (e) { res.writeHead(500); res.end(JSON.stringify({ error: e.message })); console.error(e) }
})
await new Promise(resolve => server.listen(apiPort, "127.0.0.1", resolve))
const open = async (rows, lang = "zh-CN", width = 375) => {
  const f = current = { rows, probes: 0, installs: 0, deviceGates: [], stateGates: [] }
  const page = await browser.newPage({ viewport: { width, height: 900 }, locale: lang, reducedMotion: "reduce" })
  page.setDefaultTimeout(12000)
  f.errors = []; page.on("pageerror", e => f.errors.push(e.message))
  await page.addInitScript(lang => { localStorage.setItem("steve-lang", lang); localStorage.setItem("steve-onboarded", "1") }, lang)
  await page.goto(base + "/#/devices/nodes/local")
  await page.locator("[data-capabilities]").evaluate(el => { el.open = true })
  await page.locator("[data-runtime]").first().waitFor()
  f.page = page
  return f
}
const card = (p, id = "registry-fixture") => p.locator(`[data-runtime="${id}"]`)
const fits = async p => assert.ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
const shot = async (p, name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png`, fullPage: true }) }
try {
  // Exact review rows are kept external to Git. Only the newly agreed status fields are added.
  let external = runtime("ready", { setup: { can_install: false, components: [{ id: "adapter", name: "External adapter", package: "@example/acp", installed: false, status: "unknown", path: "/fixture/external/wrapper" }] } })
  if (process.env.STEVE_REVIEW_ROWS) {
    const rows = JSON.parse(await readFile(process.env.STEVE_REVIEW_ROWS, "utf8"))
    external = clone(rows.find(r => r.id === "pi"))
    assert.equal(external.ok, true); assert.equal(external.setup.state, "ready")
    external.setup.components.forEach(c => { c.status = c.installed ? "installed" : "unknown" })
    observations.push({ source: "Review actual probe JSON replay; only optional component.status added for the revised contract", runtime: external })
  }
  const discovered = runtime("not_installed", { id: "discovered", ok: false, setup: { components: [
    { id: "agent", name: "Agent", package: "@example/agent", installed: false, status: "missing", path: "/fixture/managed/" + "target/".repeat(12) + "bin/agent", detected: { path: "/fixture/global/" + "actual/".repeat(12) + "bin/agent", version: "1.5.2" } },
    { id: "adapter", name: "ACP adapter", package: "@example/adapter", installed: false, status: "missing", path: "/fixture/managed/adapter" },
  ] } })
  const ready = runtime("ready", { id: "ready-noop" })
  const provider = runtime("unavailable", { id: "provider-only", ok: false, error: "Provider credentials missing: configure a model provider" })
  const failed = runtime("unavailable", { id: "installer-error", ok: false, setup: { error: "npm installer offline; retry permitted" } })
  const interrupted = runtime("unknown", { id: "interrupted", ok: false })
  for (const lang of ["zh-CN", "en"]) for (const width of [320, 375]) {
    const f = await open([external, discovered, ready, provider, failed, interrupted], lang, width), p = f.page
    try {
      const ext = card(p, external.id), component = ext.locator('[data-runtime-component="adapter"]')
      assert.equal(await component.locator("[data-component-status]").getAttribute("data-component-status"), "unknown")
      assert.match(await component.innerText(), lang === "zh-CN" ? /未核验/ : /Not verified/)
      assert.doesNotMatch(await component.innerText(), /未安装|Not installed/)
      assert.equal(await component.locator("[data-component-evidence]").count(), 1, "unknown explanation survives hidden ready help")
      assert.equal(await ext.locator("[data-runtime-use]").count(), 1, "successful external command is usable")
      assert.equal(await ext.locator("[data-runtime-help]").count(), 0)
      const d = card(p, "discovered").locator('[data-runtime-component="agent"]')
      assert.equal(await d.locator("[data-component-status]").getAttribute("data-component-status"), "missing")
      assert.match(await d.locator("[data-component-detected]").innerText(), /1\.5\.2/)
      assert.match(await d.locator("[data-component-detected]").innerText(), /\/fixture\/global\//)
      assert.match(await d.locator("[data-component-target]").innerText(), /\/fixture\/managed\//)
      assert.match(await d.innerText(), lang === "zh-CN" ? /其他安装不代表当前目标已完整/ : /does not complete the current target/)
      assert.equal(await card(p, "discovered").locator("[data-runtime-use]").count(), 0)
      for (const id of ["ready-noop", "provider-only"]) assert.equal(await card(p, id).locator("[data-runtime-install]").count(), 0)
      assert.equal(await card(p, "ready-noop").locator("[data-runtime-help]").count(), 0)
      assert.match(await card(p, "provider-only").innerText(), /Provider credentials missing/)
      assert.match(await card(p, "provider-only").locator("[data-runtime-help]").innerText(), /Review provider configuration/)
      assert.equal(await p.locator("[data-runtime-refresh]").isEnabled(), true)
      for (const id of ["installer-error", "interrupted", "discovered"]) {
        const button = card(p, id).locator("[data-runtime-install]")
        await button.scrollIntoViewIfNeeded(); assert.equal(await button.isEnabled(), true)
      }
      await card(p, "installer-error").locator("[data-runtime-install]").click()
      await card(p, "installer-error").locator('[data-runtime-state="installing"]').waitFor()
      assert.equal(f.installs, 1, "real installer failure offers an effective retry")
      // Reopen the full matrix for the screenshot after exercising the retry.
      f.rows = [external, discovered, ready, provider, failed, interrupted]
      await p.locator("[data-runtime-refresh]").click()
      await card(p, external.id).waitFor()
      await fits(p); await shot(p, `evidence-${lang}-${width}`)
      assert.deepEqual(f.errors, [])
    } finally { await p.close() }
  }
  console.log("PASS: R4 probe JSON + optional status replay; unknown evidence remains while ready, R3 detected path/version + separate missing managed target; ready/provider-only no no-op install; installer error retry, interrupted retry; zh/en × 320/375px")

  for (const phase of ["devices", "state"]) {
    const missing = runtime("not_installed", { ok: false, setup: { components: [{ id: "adapter", name: "ACP", package: "@example/acp", installed: false, status: "missing" }] } })
    const f = await open([missing]), p = f.page
    const old = gate([runtime("ready")]), fresh = gate(), staleState = gate([runtime("ready")])
    try {
      if (phase === "devices") { f.deviceGates.push(old); staleState.release.resolve(); f.stateGates.push(staleState) }
      else f.stateGates.push(old)
      await p.locator("[data-runtime-refresh]").click()
      await entered(old)
      f.deviceGates.push(fresh)
      const accepted = p.waitForResponse(r => r.url().endsWith("/install") && r.request().method() === "POST")
      await card(p).locator("[data-runtime-install]").click()
      await accepted
      old.release.resolve()
      await entered(fresh) // Fresh post-install request began: the stale load has already completed.
      const c = card(p)
      await c.locator('[data-runtime-state="ready"]').waitFor()
      assert.equal(await c.locator("[data-runtime-install]").isDisabled(), true, "stale ready must not clear pending")
      assert.equal(await c.locator("[data-runtime-use]").count(), 0, "cannot start while installing")
      assert.match(await c.innerText(), /正在提交安装请求|安装状态会自动刷新/)
      await shot(p, `race-${phase}-stale-ready`)
      fresh.release.resolve()
      await c.locator('[data-runtime-state="installing"]').waitFor()
      f.rows = [runtime("ready")]
      await p.waitForFunction(() => !document.querySelector("[data-runtime-refresh]")?.disabled)
      await p.locator("[data-runtime-refresh]").click()
      await c.locator('[data-runtime-state="ready"]').waitFor()
      await c.locator("[data-runtime-install]").waitFor({ state: "detached" })
      assert.equal(await c.locator("[data-runtime-use]").count(), 1)
      assert.ok(f.probes >= 3)
      assert.deepEqual(f.errors, [])
      observations.push({ scenario: `pre-install ${phase} read returns stale ready`, result: "pending retained; fresh post-install probe sees installing; ready clears pending only after completion", probes: f.probes, installs: f.installs })
    } finally { old.release.resolve(); fresh.release.resolve(); await p.close() }
  }
  console.log("PASS: deterministic HTTP gates for old devices and old state responses after install acceptance; stale ready preserves pending/disabled install; new fresh probe + polling recover; no timing sleeps")
} finally {
  if (shots) await writeFile(`${shots}/http-observations.json`, JSON.stringify({ data_source: "Running Web with isolated simulated HTTP API; no real Pi", observations, requests }, null, 2))
  await browser.close()
  await new Promise(resolve => server.close(resolve))
}
