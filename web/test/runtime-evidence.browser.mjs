// Running Web + isolated HTTP API fixtures against the unified local <NodeHarnesses> view.
// Verifies R3/R4 component evidence, detected path vs target path, and deterministic HTTP gates for stale probe races.
import assert from "node:assert/strict"
import http from "node:http"
import { readFile, mkdir, writeFile } from "node:fs/promises"
import { createWebProxy, OFFLINE_CHROMIUM_ARGS } from "./fixtures/hub-proxy.mjs"
import { emptySnapshot } from "./fixtures/wire-views.ts"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const apiPort = Number(process.env.STEVE_TEST_API_PORT || 0)
const shots = process.env.STEVE_TEST_SHOTS
if (shots) await mkdir(shots, { recursive: true })
const browser = await chromium.launch({
  headless: true,
  args: OFFLINE_CHROMIUM_ARGS,
  ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}),
})
const ws = { id: "fixture", name: "HTTP fixture", description: "Simulated API", item_prefix: "ST", sort: 0, created: 0 }
const device = { id: "hub", name: "HTTP fixture Hub", roles: ["Hub"], node_id: "local", connected: true, can_configure: true, revision: 0, os: "fixture", version: "fixture", status: "正常", max_sessions: 4, seen: 0, created: 0 }
const runtime = (state, extra = {}) => ({ id: "registry-fixture", name: "Registry fixture", ok: state === "ready", version: "", path: "/fixture/adapter", models: [], note: "HTTP fixture; no real Pi process", modes: ["full_access"], install_hint: "Generic: install components first", ...extra,
  setup: { state, can_install: true, components: ["agent", "adapter"].map(id => ({ id, name: id, package: "@example/" + id, status: "installed", path: "/fixture/managed/" + id })), help: "Review provider configuration on the Node; recheck discovery", ...extra.setup } })
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
const streams = new Set()
const server = http.createServer(async (req, res) => {
  try {
    const f = current, url = new URL(req.url, "http://fixture"), p = url.pathname
    let bytes = ""; for await (const chunk of req) bytes += chunk
    requests.push({ method: req.method, path: req.url, body: bytes ? JSON.parse(bytes) : null })
    if (p === "/api/stream") {
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store" })
      res.write(": keepalive\n\n")
      streams.add(res)
      req.on("close", () => streams.delete(res))
      return
    }
    let data
    if (p === "/auth/hello") data = { name: "steve", version: "fixture", api: 2, login: { token: false, oidc: false } }
    else if (p === "/api/shell") {
      data = { version: "fixture", web_version: "fixture", owner: false, workspace: ws, workspaces: [ws], bots: [], allBots: [], projects: [], tags: [], settings: { onboard: "1" } }
    } else if (p === "/api/snapshot") data = emptySnapshot()
    else if (p === "/api/summary") data = { thinking: false, hold: null }
    else if (p === "/api/devices") data = { devices: [device] }
    else if (p === "/api/runtimes") {
      if (url.searchParams.get("fresh") !== "1") { res.writeHead(200, { "content-type": "application/json", "cache-control": "no-store" }); res.end(JSON.stringify(clone(f.rows))); return }
      f.probes++
      const g = f.probeGates.shift()
      data = clone(g?.rows || f.rows)
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
    else if (p === "/api/notifications") data = []
    else if (p.endsWith("/events")) data = []
    else { res.writeHead(404, { "content-type": "application/json", "cache-control": "no-store" }); res.end("{}"); return }
    res.writeHead(200, { "content-type": "application/json", "cache-control": "no-store" }); res.end(JSON.stringify(data))
  } catch (e) { res.writeHead(500); res.end(JSON.stringify({ error: e.message })); console.error(e) }
})
await new Promise(resolve => server.listen(apiPort, "127.0.0.1", resolve))
const apiBase = `http://127.0.0.1:${server.address().port}`
const web = await createWebProxy({ hubUrl: apiBase, cacheAssets: true })
const base = web.url
const contexts = new Map()
const contextFor = async (lang) => {
  let ctx = contexts.get(lang)
  if (!ctx) {
    ctx = await browser.newContext({ locale: lang, reducedMotion: "reduce" })
    await ctx.addInitScript(l => { localStorage.setItem("steve-lang", l); localStorage.setItem("steve-onboarded", "1") }, lang)
    contexts.set(lang, ctx)
  }
  return ctx
}
const open = async (rows, lang = "zh-CN", width = 375) => {
  const f = current = { rows, probes: 0, installs: 0, probeGates: [] }
  const ctx = await contextFor(lang)
  const page = await ctx.newPage()
  await page.setViewportSize({ width, height: 900 })
  page.setDefaultTimeout(12000)
  f.errors = []; page.on("pageerror", e => f.errors.push(e.message))
  await page.goto(base + "/#/devices/nodes/local/harness")
  await page.locator('[data-node-harnesses="local"]').waitFor()
  await page.locator("[data-node-harness-id], [data-harness-section-toggle]").first().waitFor()
  f.page = page
  return f
}
const card = (p, id = "registry-fixture") => p.locator(`[data-node-harness-id="${id}"]`)
const fits = async p => assert.ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
const shot = async (p, name) => { if (shots) await p.screenshot({ path: `${shots}/${name}.png`, fullPage: true }) }
try {
  let external = runtime("ready", { setup: { can_install: false, components: [{ id: "adapter", name: "External adapter", package: "@example/acp", status: "unknown", path: "/fixture/external/wrapper" }] } })
  if (process.env.STEVE_REVIEW_ROWS) {
    const rows = JSON.parse(await readFile(process.env.STEVE_REVIEW_ROWS, "utf8"))
    external = clone(rows.find(r => r.id === "pi"))
    assert.equal(external.ok, true); assert.equal(external.setup.state, "ready")
    observations.push({ source: "Review actual probe JSON replay", runtime: external })
  }
  const discovered = runtime("not_installed", { id: "discovered", ok: false, setup: { components: [
    { id: "agent", name: "Agent", package: "@example/agent", status: "missing", path: "/fixture/managed/" + "target/".repeat(12) + "bin/agent", detected: { path: "/fixture/global/" + "actual/".repeat(12) + "bin/agent", version: "1.5.2" } },
    { id: "adapter", name: "ACP adapter", package: "@example/adapter", status: "missing", path: "/fixture/managed/adapter" },
  ] } })
  const ready = runtime("ready", { id: "ready-noop" })
  const provider = runtime("unavailable", { id: "provider-only", ok: false, error: "Provider credentials missing: configure a model provider" })
  const failed = runtime("unavailable", { id: "installer-error", ok: false, setup: { error: "npm installer offline; retry permitted" } })
  const interrupted = runtime("unknown", { id: "interrupted", ok: false })
  for (const lang of ["zh-CN", "en"]) for (const width of [320, 375]) {
    const f = await open([external, discovered, ready, provider, failed, interrupted], lang, width), p = f.page
    try {
      const ext = card(p, external.id), component = ext.locator('[data-node-harness-component="adapter"]')
      const foldBtn = p.locator('[data-harness-section-toggle="not_installed"]')
      if (await foldBtn.count() && await foldBtn.getAttribute("aria-expanded") === "false") await foldBtn.click()
      for (const r of [external, discovered, provider]) await card(p, r.id).locator("[data-node-harness-details] summary").click()
      assert.equal(await component.locator("[data-component-status]").getAttribute("data-component-status"), "unknown")
      assert.match(await component.innerText(), lang === "zh-CN" ? /未核验/ : /Not verified/)
      assert.doesNotMatch(await component.innerText(), /未安装|Not installed/)
      assert.equal(await component.locator("[data-component-evidence]").count(), 1, "unknown explanation survives hidden ready help")
      assert.equal(await ext.locator(`[data-harness-use="${external.id}"]`).count(), 1, "successful external command is usable")
      assert.equal(await ext.locator("[data-harness-help]").count(), 0)
      const d = card(p, "discovered").locator('[data-node-harness-component="agent"]')
      assert.equal(await d.locator("[data-component-status]").getAttribute("data-component-status"), "missing")
      assert.match(await d.locator("[data-component-detected]").innerText(), /1\.5\.2/)
      assert.match(await d.locator("[data-component-detected]").innerText(), /\/fixture\/global\//)
      assert.match(await d.locator("[data-component-target]").innerText(), /\/fixture\/managed\//)
      assert.match(await d.innerText(), lang === "zh-CN" ? /其他安装不代表当前目标已完整/ : /does not complete the current target/)
      assert.equal(await card(p, "discovered").locator("[data-harness-use]").count(), 0)
      for (const id of ["ready-noop", "provider-only"]) assert.equal(await card(p, id).locator("[data-harness-install]").count(), 0)
      assert.equal(await card(p, "ready-noop").locator("[data-harness-help]").count(), 0)
      assert.match(await card(p, "provider-only").innerText(), /Provider credentials missing/)
      assert.match(await card(p, "provider-only").locator("[data-harness-help]").innerText(), /Review provider configuration/)
      assert.equal(await p.locator("[data-node-harness-refresh]").isEnabled(), true)
      for (const id of ["installer-error", "interrupted", "discovered"]) {
        const button = card(p, id).locator(`[data-harness-install="${id}"]`)
        await button.scrollIntoViewIfNeeded(); assert.equal(await button.isEnabled(), true)
      }
      await card(p, "installer-error").locator('[data-harness-install="installer-error"]').click()
      await card(p, "installer-error").locator('[data-node-harness-state="installing"]').waitFor()
      assert.equal(f.installs, 1, "real installer failure offers an effective retry")
      f.rows = [external, discovered, ready, provider, failed, interrupted]
      const refreshed = p.waitForResponse(r => new URL(r.url()).pathname === "/api/runtimes" && new URL(r.url()).searchParams.get("fresh") === "1")
      await p.locator("[data-node-harness-refresh]").click()
      await refreshed
      await card(p, "installer-error").locator('[data-node-harness-state="unavailable"]').waitFor()
      await card(p, external.id).waitFor()
      await fits(p); await shot(p, `evidence-${lang}-${width}`)
      assert.deepEqual(f.errors, [])
    } finally { await p.close() }
  }
  console.log("PASS: R3/R4 component evidence on unified NodeHarnesses; unknown evidence survives ready, detected vs managed target, installer retry; zh/en × 320/375px")

  {
    const missing = runtime("not_installed", { ok: false, setup: { components: [{ id: "adapter", name: "ACP", package: "@example/acp", status: "missing" }] } })
    const f = await open([missing]), p = f.page
    const old = gate([runtime("ready")]), fresh = gate()
    try {
      await p.locator('[data-harness-section-toggle="not_installed"]').waitFor()
      f.probeGates.push(old)
      await p.locator("[data-node-harness-refresh]").click()
      await entered(old)
      await p.locator('[data-harness-section-toggle="not_installed"]').click()
      f.probeGates.push(fresh)
      const accepted = p.waitForResponse(r => r.url().endsWith("/install") && r.request().method() === "POST")
      await card(p).locator('[data-harness-install="registry-fixture"]').click()
      await accepted
      old.release.resolve()
      await entered(fresh)
      const c = card(p)
      await c.locator('[data-node-harness-state="installing"]').waitFor()
      assert.equal(await c.locator('[data-harness-install="registry-fixture"]').isDisabled(), true, "stale ready must not clear pending")
      assert.equal(await c.locator("[data-harness-use]").count(), 0, "cannot start while installing")
      assert.match(await c.locator("[data-harness-installing-status]").innerText(), /正在提交安装请求|安装状态会自动刷新/)
      await shot(p, "race-stale-runtime")
      fresh.release.resolve()
      await c.locator('[data-node-harness-state="installing"]').waitFor()
      f.rows = [runtime("ready")]
      await p.waitForFunction(() => !document.querySelector("[data-node-harness-refresh]")?.disabled)
      await p.locator("[data-node-harness-refresh]").click()
      await c.locator('[data-node-harness-state="ready"]').waitFor()
      await c.locator('[data-harness-install="registry-fixture"]').waitFor({ state: "detached" })
      assert.equal(await c.locator('[data-harness-use="registry-fixture"]').count(), 1)
      assert.ok(f.probes >= 3)
      assert.deepEqual(f.errors, [])
      observations.push({ scenario: "pre-install runtime probe returns stale ready", result: "pending retained; fresh post-install probe sees installing; ready clears pending only after completion", probes: f.probes, installs: f.installs })
    } finally { old.release.resolve(); fresh.release.resolve(); await p.close() }
  }
  console.log("PASS: deterministic HTTP gates for stale probe race after install acceptance on unified NodeHarnesses")
} finally {
  if (shots) await writeFile(`${shots}/http-observations.json`, JSON.stringify({ data_source: "Running Web with isolated simulated HTTP API; no real Pi", observations, requests }, null, 2))
  for (const s of streams) s.end()
  for (const ctx of contexts.values()) await ctx.close()
  await browser.close()
  await web.close()
  server.closeAllConnections()
  await new Promise(resolve => server.close(resolve))
}
