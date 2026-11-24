// Real device components, isolated API fixtures. Vendor packages and a remote machine are not used.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
import { chromium } from "playwright"
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:19421"
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1280, height: 950 } })
page.setDefaultTimeout(12000)
const errors = [], writes = []
page.on("pageerror", e => errors.push(e.stack || e.message))
const node = id => ({ node_id: id, name: id, state: "active", linked: true, local: id === "local", running: 0, waiting: 0, max_sessions: 4, fingerprint: "fixture", upgrade: { reason: "manual", can: false } })
const component = (id, installed, managed = true, state = "available") => ({ id, name: id === "cli" ? "Fixture CLI" : "Fixture ACP", package: "@example/" + id, managed, state, installed, latest: "2.0.0", checked: Date.now(), reason: managed ? null : "External prefix is read-only" })
const report = (id, version) => ({ report: { node: id, checking: false, harnesses: [{ harness: "fixture", name: "Fixture Runtime", can_upgrade: true, components: [component("cli", version, false), component("adapter", version)] }] }, runs: {}, at: Date.now(), error: null })
const data = { local: report("local", "1.0.0"), remote: report("remote", "1.5.0") }
let holdPost, releasePost, failPost = false
await page.route("**/*", async route => {
  const req = route.request(), u = new URL(req.url()), p = u.pathname
  if (u.origin !== base) return route.abort("blockedbyclient")
  if (!p.startsWith("/api/")) return route.continue()
  if (p.startsWith("/api/node-harness-updates/")) {
    const [, , , id, harness] = p.split("/")
    if (req.method() === "POST") {
      assert.equal(id, "remote"); assert.equal(harness, "fixture")
      const body = req.postDataJSON(); writes.push(body)
      assert.deepEqual(body.versions, { adapter: "2.0.0" }, "external CLI is not a remote write target")
      if (holdPost) await holdPost
      if (failPost) return route.fulfill({ status: 403, json: { error: "Node policy denies exec" } })
      const receipt = { operation: body.operation, state: { state: "running" } }
      data.remote.runs.fixture = receipt
      return route.fulfill({ json: { receipt } })
    }
    return route.fulfill({ json: data[id] })
  }
  const response = p === "/api/nodes" ? { nodes: [node("local"), node("remote")] }
    : p === "/api/node-harnesses" ? { nodes: Object.fromEntries(["local", "remote"].map(id => [id, { state: "reported", stale: false, refreshing: false, checked_at: Date.now(), harnesses: [{ id: "fixture", name: "Fixture Runtime", available: true, version: id === "local" ? "1.0.0" : "1.5.0", adapter_version: "1.0.0", models: [id + "/model"] }] }])) }
    : p === "/api/devices" ? { devices: [{ id: "hub", name: "Local", roles: ["Hub"], node_id: "local", connected: true, status: "正常", max_sessions: 4 }], runtimes: [] }
    : p === "/api/vitals" ? { nodes: {} } : p === "/api/node-context" ? { owner_context: "off" }
    : p === "/api/nodes/upgrade-default" ? { policy: "notify", drain_secs: 1800, health_secs: 180 }
    : p === "/api/access" ? { clients: [], tokens: [], current: null } : null
  if (p.includes("contacts") || p.startsWith("/api/people")) return route.fulfill({ status: 404, json: {} })
  return route.fulfill({ status: response ? 200 : 404, json: response ?? {} })
})
const panel = () => page.locator("[data-harness-updates]")
const open = async id => {
  await page.goto(base + "/test/fixtures/devices.html?strict#/devices/nodes/" + id + "/harness")
  await panel().locator("[data-harness-update=fixture]").waitFor()
  await page.waitForFunction(id => document.querySelector(`[data-harness-updates="${id}"] [data-harness-update=fixture]`)?.textContent.includes(id === "local" ? "1.0.0" : "1.5.0"), id)
  await page.locator(`[data-node-harnesses="${id}"] [data-node-harness-models]`).getByText(id + "/model", { exact: true }).waitFor()
}
const reread = () => page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")))
try {
  await open("local")
  assert.match(await panel().innerText(), /1\.0\.0/)
  await open("remote")
  assert.match(await panel().innerText(), /1\.5\.0/)
  assert.doesNotMatch(await panel().innerText(), /1\.0\.0/)
  assert.match(await panel().innerText(), /有新版本/)
  holdPost = new Promise(resolve => { releasePost = resolve })
  const upgrade = () => panel().locator("[data-harness-upgrade=fixture]")
  await upgrade().click()
  await page.waitForFunction(() => document.querySelector("[data-harness-upgrade=fixture]")?.disabled)
  await page.waitForTimeout(150)
  assert.equal(writes.length, 1, "duplicate clicks cannot dispatch another command")
  assert.equal(await panel().locator("[data-harness-upgrade-result=succeeded]").count(), 0)
  holdPost = null; releasePost()
  await panel().locator("[data-harness-upgrade-result=running]").waitFor()
  data.remote.runs.fixture.state = { state: "unknown", reason: "Link lost after acceptance" }
  data.remote.error = "Node offline"; data.remote.code = "offline"
  await reread()
  await panel().locator("[data-harness-upgrade-result=unknown]").waitFor()
  assert.equal(await upgrade().isDisabled(), true)
  assert.equal(await panel().locator("[data-harness-upgrade-result=succeeded]").count(), 0)
  data.remote.error = null; data.remote.code = null
  data.remote.runs.fixture.state = { state: "finished", outcome: "succeeded" }
  data.remote.report.harnesses[0].components[1] = component("adapter", "2.0.0", true, "current")
  await reread()
  await panel().locator("[data-harness-upgrade-result=succeeded]").waitFor()
  await page.reload()
  await panel().locator("[data-harness-upgrade-result=succeeded]").waitFor()
  assert.equal(writes.length, 1, "refresh only reads the persisted receipt")
  // A policy rejection remains a visible failure and may be retried after the cause is fixed.
  data.remote.report.harnesses[0].components[1] = component("adapter", "1.5.0")
  data.remote.runs.fixture.state = { state: "finished", outcome: "failed", error: { code: "denied", safe_detail: "Node policy denies exec" } }
  failPost = true
  await reread(); await panel().locator("[data-harness-upgrade-result=failed]").waitFor()
  await upgrade().click()
  await panel().locator("[data-harness-update-error]").waitFor()
  assert.equal(writes.length, 2)
  // An old Node explicitly asks for a Node upgrade, without an enabled package write.
  data.remote.report = null; data.remote.error = "unsupported harness-updates-v1"; data.remote.code = "unsupported"
  await reread()
  await panel().getByText(/此 Node 尚不支持组件更新/).waitFor()
  assert.equal(await panel().locator("[data-harness-upgrade]").count(), 0)
  for (const width of [1280, 390]) {
    data.remote.report = report("remote", "1.5.0").report; data.remote.error = null; data.remote.code = null
    await reread(); await panel().locator("[data-harness-update=fixture]").waitFor()
    await page.setViewportSize({ width, height: 950 })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no horizontal overflow")
    if (process.env.STEVE_TEST_SHOTS) { await mkdir(process.env.STEVE_TEST_SHOTS, { recursive: true }); await page.screenshot({ path: process.env.STEVE_TEST_SHOTS + `/harness-updates-${width}.png`, fullPage: true }) }
  }
  assert.deepEqual(errors, [])
  console.log("harness updates browser: passed (Node identity, duplicate click, running/unknown/failure/success, refresh, old Node, narrow layout)")
} catch (e) {
  console.error({ errors, body: await page.locator("body").innerText() })
  throw e
} finally { await browser.close() }
