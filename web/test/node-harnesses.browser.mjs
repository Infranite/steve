// Device pages with deterministic per-Node API reports; no installed Hub or provider is contacted.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1360, height: 980 } })
const errors = [], writes = []
page.on("pageerror", e => errors.push(e.message))
const node = (id, extra = {}) => ({ node_id: id, name: id, fingerprint: "1234", state: "active", linked: true, local: id === "local", max_sessions: 4, running: 0, waiting: 0, ...extra })
const h = (version, model, extra = {}) => ({ id: "fixture-adapter", name: "Fixture adapter", available: true, version, adapter_version: "adapter-2", models: [model], degraded_reason: null, installed: true, setup: null, error: null, ...extra })
const r = (harnesses = [], extra = {}) => ({ state: "reported", checked_at: Date.now(), attempted_at: Date.now(), stale: false, refreshing: false, error: null, harnesses, ...extra })
let nodes = [node("local"), node("remote"), node("empty"), node("old"), node("offline", { linked: false }), node("failed"), node("pending-report")]
let reports = {
  local: r([h("local-cli", "local/model")]), remote: r([h("remote-cli", "remote/model"), h("auth-cli", "", { id: "auth", name: "Auth adapter", available: false, installed: true, models: [], degraded_reason: "authentication_required" }), h("", "", { id: "path-mismatch", name: "Path mismatch adapter", available: false, installed: true, models: [], degraded_reason: "command_not_in_service_path", missing_command: { command: "agy", service_path: "/usr/bin:/bin:/usr/sbin:/sbin", login_shell_path: "/Users/example/.local/bin/agy", login_shell: "/bin/zsh", refresh_command: "steve node refresh-env" } }), h("", "", { id: "absent", name: "Absent adapter", available: false, installed: false, models: [], setup: { state: "not_installed", components: [{ id: "cli", name: "CLI", package: "fixture", status: "missing" }], can_install: false, reason: "fixture missing binary" } })]),
  empty: r(), old: r([], { state: "unsupported", checked_at: null, error: "fixture unsupported method" }),
  offline: r([h("cached-cli", "cached/model")], { state: "offline", stale: true }),
  failed: r([h("last-cli", "last/model")], { state: "failed", stale: true, error: "fixture probe failed" }),
  "pending-report": r([], { state: "unreported", checked_at: null }),
}
let refreshes = 0, failedRead = false, release
const json = (route, body, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })
await page.route("**/*", async route => {
  const req = route.request(), url = new URL(req.url())
  if (url.origin !== base) return route.abort("blockedbyclient")
  if (!url.pathname.startsWith("/api/")) return route.continue()
  if (req.method() !== "GET") writes.push(req.method() + " " + url.pathname)
  if (url.pathname === "/api/nodes") return json(route, { nodes })
  if (url.pathname === "/api/devices") return json(route, { devices: [], runtimes: [] })
  if (url.pathname === "/api/node-harnesses") return failedRead ? json(route, { error: "fixture report unavailable" }, 503) : json(route, { nodes: reports })
  if (url.pathname === "/api/node-harnesses/remote") {
    assert.equal(url.searchParams.get("fresh"), "1")
    refreshes++
    await new Promise(resolve => { release = resolve })
    reports.remote = r([h("refreshed-cli", "new/model")])
    return json(route, { node_id: "remote", report: reports.remote })
  }
  if (url.pathname === "/api/vitals") return json(route, { nodes: {} })
  if (url.pathname === "/api/nodes/upgrade-default") return json(route, { policy: "notify" })
  if (url.pathname === "/api/node-context") return json(route, { owner_context: "off" })
  if (url.pathname === "/api/access") return json(route, { tokens: [], clients: [] })
  return json(route, {}, 404)
})
const reread = () => page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")))
const summary = id => page.locator(`[data-node-row="${id}"] [data-node-harness]`)
try {
  await page.goto(base + "/test/fixtures/devices.html?strict#/devices/nodes")
  await summary("remote").getByText("可用：Fixture adapter remote-cli", { exact: true }).waitFor()
  assert.equal(await summary("local").innerText(), "可用：Fixture adapter local-cli")
  assert.match(await summary("empty").innerText(), /没有可用/)
  assert.equal(await summary("old").innerText(), "此 Node 不支持 harness 探测")
  assert.match(await summary("offline").innerText(), /离线.*过期/)
  assert.match(await summary("failed").innerText(), /探测失败.*上次/)
  assert.match(await summary("pending-report").innerText(), /尚未报告/)
  if (process.env.STEVE_TEST_SHOTS) {
    await mkdir(process.env.STEVE_TEST_SHOTS, { recursive: true })
    await page.screenshot({ path: process.env.STEVE_TEST_SHOTS + "/node-harnesses-list.png", fullPage: true, animations: "disabled" })
  }
  await page.locator('[data-node-row="remote"]').click()
  await page.locator('[data-node-action="harnessAuth"]').waitFor()
  await page.locator('[data-node-tab="harness"]').click()
  await page.waitForFunction(() => location.hash === "#/devices/nodes/remote/harness")
  const detail = page.locator('[data-node-harnesses="remote"]')
  await detail.locator('[data-node-harness-models]').getByText("remote/model", { exact: true }).waitFor()
  assert.doesNotMatch(await detail.innerText(), /local-cli|local\/model|Hub-only harness/)
  assert.match(await detail.locator("[data-node-harness-versions]").first().innerText(), /CLI remote-cli[\s\S]*ACP adapter-2/)
  await detail.locator('[data-node-harness-id="auth"] [data-node-harness-degraded]').waitFor()
  const auth = detail.locator('[data-node-harness-id="auth"]')
  await auth.getByText("需要身份验证", { exact: true }).waitFor()
  assert.match(await auth.locator("[data-node-harness-degraded]").innerText(), /非交互凭据/)
  const pathMismatch = detail.locator('[data-node-harness-id="path-mismatch"]')
  await pathMismatch.locator("[data-node-harness-missing-command]").waitFor()
  assert.match(await pathMismatch.locator("[data-node-harness-degraded]").innerText(), /服务 PATH 中找不到该命令/)
  assert.equal(await pathMismatch.locator("[data-missing-command-login-path]").innerText(), "/Users/example/.local/bin/agy")
  assert.equal(await pathMismatch.locator("[data-missing-command-service-path]").innerText(), "/usr/bin:/bin:/usr/sbin:/sbin")
  assert.equal(await pathMismatch.locator("[data-missing-command-refresh]").innerText(), "steve node refresh-env")
  await pathMismatch.locator('[data-harness-refresh-env-copy="path-mismatch"]').waitFor()
  // Uninstalled harnesses stay folded in the Not installed section until expanded.
  assert.equal(await detail.locator('[data-node-harness-id="absent"]').count(), 0)
  await detail.locator('[data-harness-section-toggle="not_installed"]').click()
  await detail.locator('[data-node-harness-id="absent"][data-node-harness-state="not_installed"]').waitFor()
  assert.equal(await detail.locator("[data-node-harness-manage]").count(), 0, "remote inspection has no local maintenance actions")
  assert.equal(await detail.getByText("fixture missing binary", { exact: true }).isVisible(), false)
  await detail.locator('[data-node-harness-id="absent"] summary').click()
  await detail.getByText("fixture missing binary", { exact: true }).waitFor()
  const refresh = detail.locator("[data-node-harness-refresh]")
  await refresh.click()
  await page.waitForFunction(() => document.querySelector("[data-node-harness-refresh]")?.disabled)
  assert.equal(refreshes, 1)
  release()
  await detail.getByText("new/model", { exact: true }).waitFor()
  assert.equal(refreshes, 1)
  assert.match(await detail.innerText(), /refreshed-cli/)
  if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: process.env.STEVE_TEST_SHOTS + "/node-harnesses-detail.png", fullPage: true, animations: "disabled" })
  // Losing the connection retains evidence, marked as an old report; stale models tuck into Details.
  nodes = nodes.map(n => n.node_id === "remote" ? { ...n, linked: false } : n)
  reports.remote = { ...reports.remote, state: "offline", stale: true }
  await reread()
  await detail.getByText("Node 离线，harness 数据可能过期", { exact: true }).waitFor()
  await detail.locator('[data-node-harness-state="stale"]').waitFor()
  assert.equal(await refresh.isDisabled(), true)
  await detail.locator('[data-node-harness-id="fixture-adapter"] [data-node-harness-details] summary').click()
  await detail.locator('[data-node-harness-reported-models]').waitFor()
  assert.match(await detail.locator('[data-node-harness-reported-models]').innerText(), /上次报告的模型/)
  assert.match(await detail.locator('[data-node-harness-reported-models]').innerText(), /new\/model/)
  failedRead = true
  await reread()
  await detail.locator("[data-node-harness-error]").waitFor()
  assert.match(await detail.innerText(), /new\/model/, "failed inventory reads keep the last good report")
  await page.setViewportSize({ width: 390, height: 844 })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "detail fits mobile")
  if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: process.env.STEVE_TEST_SHOTS + "/node-harnesses-mobile.png", fullPage: true, animations: "disabled" })
  failedRead = false
  await page.evaluate(() => { location.hash = "#/devices/nodes" })
  await summary("remote").waitFor()
  assert.equal(await summary("remote").isVisible(), true, "harness summary is visible on mobile")
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
  assert.deepEqual(writes, [], "probes do not change provider, login, installation or pairing")
  assert.deepEqual(errors, [])
  process.stdout.write("Node harnesses: per-Node reports, versions, models, absence, old/unreported/failure/offline, refresh and mobile passed\n")
} finally { await browser.close() }
