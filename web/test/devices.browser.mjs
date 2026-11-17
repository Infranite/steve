// The real devices pages with an isolated owner API. Run against Vite, with external Playwright.
import assert from "node:assert/strict"
import { mkdir } from "node:fs/promises"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1360, height: 980 } })
const errors = [], writes = []
page.on("pageerror", e => errors.push(e.message))
const remote = { node_id: "fixture-remote", name: "Remote laptop", fingerprint: "9e3f5a07c21b84d60f9a77e3b10c4d52", state: "active", linked: true, local: false, max_sessions: 32, running: 32, waiting: 2, last_seen: Date.now() - 60_000,
  host: { os: "macos", os_version: "26.1", arch: "aarch64", cpus: 12, mem_total: 36 * 2 ** 30 }, link: { route: "loopback", via: "tailcat", tls: false, since: 0, latency_ms: 38.2, latency_at: Date.now() },
  upgrade: { build: { version: "0.2.112", commit: "abc", target: "x" }, policy: "notify", own_policy: null, can: false, reason: "current", latest: null } }
const sample = { at: Date.now() - 20_000, cpus: 12, load: [6, 5, 4], mem_total: 36 * 2 ** 30, mem_available: 18 * 2 ** 30, rtt_ms: 38 }
const vitals = { "fixture-remote": { sampling: { interval_ms: 60_000, stale_after_ms: 180_000, now: Date.now() }, latest: sample, series: [{ at: Date.now() - 120_000, load: 0.4, mem: 0.5, rtt: 40 }, { at: Date.now() - 60_000, load: null, mem: 0.5, rtt: 38 }, { at: Date.now() - 20_000, load: 0.5, mem: 0.5, rtt: 38 }] } }
let nodes = [{ ...remote }, { ...remote }, { ...remote, node_id: "fixture-local", local: true }]
let status = 200, release, reads = 0
const held = new Promise(resolve => { release = resolve })
let hold = true
const json = (route, body, code = 200) => route.fulfill({ status: code, contentType: "application/json", body: JSON.stringify(body) })
await page.route("**/*", async route => {
  const request = route.request(), url = new URL(request.url())
  if (url.origin !== base) return route.abort("blockedbyclient")
  if (!url.pathname.startsWith("/api/")) return route.continue()
  if (request.method() !== "GET") writes.push(request.method() + " " + url.pathname)
  if (url.pathname === "/api/nodes/fixture-remote/limit" && request.method() === "POST") {
    const { max_sessions } = request.postDataJSON()
    assert.ok(Number.isInteger(max_sessions) && max_sessions >= 1 && max_sessions <= 256)
    nodes = nodes.map(n => n.node_id === remote.node_id ? { ...n, max_sessions } : n)
    return json(route, { ok: true })
  }
  if (url.pathname === "/api/nodes/fixture-remote/upgrade/switch" && request.method() === "POST") return json(route, { operation: "nup_fixture" }, 202)
  if (url.pathname === "/api/nodes/fixture-remote/pause" && request.method() === "POST") {
    nodes = nodes.map(n => n.node_id === remote.node_id ? { ...n, state: "paused" } : n)
    return json(route, { ok: true })
  }
  if (url.pathname === "/api/nodes") {
    reads++
    assert.equal(request.headers()["x-steve-ws"], "fixture-workspace")
    if (hold) await held
    return json(route, status === 200 ? { nodes } : { error: "fixture: node list unavailable" }, status)
  }
  if (url.pathname === "/api/vitals") return json(route, { nodes: vitals })
  if (url.pathname === "/api/nodes/upgrade-default") return json(route, { policy: "notify", hub: { version: "0.2.112", commit: "abc", target: "x" }, drain_secs: 1800, health_secs: 180 })
  if (url.pathname === "/api/node-context") return json(route, { owner_context: "off" })
  if (url.pathname === "/api/access") return json(route, { tokens: [], clients: [{ id: "c1", kind: "web", name: "This browser", platform: "macOS", version: "x", origin: null, created: 0, last_seen: Date.now(), last_route: "lan", last_ip: null, revoked: null }], current: "c1", route: "lan", hub: { version: "0.2.112" } })
  // OwnerChip's contacts probe and the device rows are deliberately not wired in this fixture.
  return json(route, {}, 404)
})
const row = () => page.locator('[data-node-row="fixture-remote"]')
/* the page reads again at once when it becomes visible, instead of waiting for its 10-second poll */
const reread = () => page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")))
try {
  await page.goto(base + "/test/fixtures/devices.html?legacy#/devices/nodes")
  await page.locator("[data-nodes]").waitFor()
  assert.equal(await page.locator("[data-node-row]").count(), 0, "loading is not an empty inventory")
  hold = false; release()
  await row().waitFor()
  assert.deepEqual(await page.locator("[data-node-row]").evaluateAll(xs => xs.map(x => x.getAttribute("data-node-row"))), ["local", "fixture-remote"], "the Hub's own Node first; duplicate identities are not extra Nodes")
  assert.equal(await page.getByText("Legacy duplicate", { exact: true }).count(), 0, "exact legacy identity is deduplicated")
  await page.locator('[data-seg="nodes"] .ui-segc__count').getByText("2", { exact: true }).waitFor()
  await page.locator('[data-seg="nodes"][data-warn]').waitFor()
  await row().locator("[data-node-status=queued]").getByText("排队 2", { exact: true }).waitFor()
  assert.equal(await page.getByText("全部在 Hub", { exact: true }).count(), 0)
  // a card: its machine as the Node reported it, CPU load over its cores, the round trip and route, sessions against the limit
  await row().locator("[data-node-host]").getByText("macOS 26.1 · arm64 · 12 核 · 36 GB", { exact: true }).waitFor()
  await row().locator('[data-metric="cpu"]').getByText("50%", { exact: true }).waitFor()
  assert.match(await row().locator('[data-metric="rtt"]').innerText(), /38\s*ms[\s\S]*tailcat/)
  assert.match(await row().locator('[data-metric="sessions"]').innerText(), /32\s*\/ 32[\s\S]*2 个在排队/)
  assert.equal(await row().locator("[data-node-alert]").count(), 0, "a Node with nothing to do shows no alert line")

  await row().click()
  await page.waitForFunction(() => location.hash === "#/devices/nodes/fixture-remote")
  const detail = page.locator("[data-node-detail]")
  await detail.locator(`[data-node-fingerprint="${remote.fingerprint}"]`).getByText("9e3f 5a07 c21b 84d6 0f9a 77e3 b10c 4d52", { exact: true }).waitFor()
  await detail.getByText("最近连接", { exact: true }).waitFor()
  await page.locator("[data-node-sub]").getByText("经 tailcat 连到 Hub", { exact: true }).waitFor()
  await detail.locator('[data-node-upgrade="current"]').getByText("与 Hub 一致", { exact: true }).waitFor()
  await detail.locator('[data-node-owner-context="inherit"]').getByText("跟随 Hub 默认（不加载）", { exact: true }).waitFor()
  await detail.locator("[data-node-trends]").waitFor()
  assert.equal(await detail.getByText("Hub-only harness", { exact: true }).count(), 0)
  assert.equal(await detail.locator("[data-owner-chip]").count(), 0)
  assert.deepEqual(writes, [], "inventory and identity inspection never mutate pairing or permissions")
  // Capacity changes are explicit saves, not new pairing or execution approvals.
  const capacity = detail.locator("[data-session-limit-input]")
  assert.equal(await capacity.getAttribute("aria-valuenow"), "32")
  assert.match(await detail.locator("[data-limit-semantics]").textContent(), /无需重启.*不打断/)
  await capacity.fill("64"); await capacity.blur()
  assert.deepEqual(writes, [], "editing a capacity does not save until requested")
  await detail.locator("[data-session-limit-save]").click()
  await detail.locator('[data-session-limit="64"]').waitFor()
  assert.deepEqual(writes, ["POST /api/nodes/fixture-remote/limit"])
  assert.equal(await detail.locator("[data-running]").getAttribute("data-running"), "32")
  assert.equal(await detail.locator("[data-running]").getAttribute("data-waiting"), "2")
  for (const width of [1440, 390]) {
    for (const dark of [false, true]) {
      await page.setViewportSize({ width, height: 980 })
      await page.evaluate(dark => document.documentElement.classList.toggle("dark", dark), dark)
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "the Node's page does not overflow")
      if (process.env.STEVE_TEST_SHOTS) { await mkdir(process.env.STEVE_TEST_SHOTS, { recursive: true }); await page.screenshot({ path: `${process.env.STEVE_TEST_SHOTS}/node-${width}-${dark ? "dark" : "light"}.png`, fullPage: true, animations: "disabled" }) }
    }
  }
  await page.setViewportSize({ width: 1360, height: 980 }); await page.evaluate(() => document.documentElement.classList.remove("dark"))
  // an upgrade waiting for running sessions: "switch now" asks that upgrade to stop waiting, never starts a second one
  const draining = { operation_id: "nup_fixture", version: "0.2.113", from_version: "0.2.112", phase: "draining", error: null, requested_by: "owner", force: false, created: Date.now() - 60_000, updated: Date.now() - 10_000 }
  nodes = nodes.map(n => n.node_id === remote.node_id ? { ...n, upgrade: { ...n.upgrade, can: false, reason: "busy", latest: draining } } : n)
  await reread()
  await detail.locator('[data-node-upgrade="running"] [data-upgrade-drain]').waitFor()
  await detail.locator("[data-node-upgrade-switch]").getByText("立即切换（中断 32 个会话）", { exact: true }).click()
  for (let i = 0; i < 100 && writes.at(-1) !== "POST /api/nodes/fixture-remote/upgrade/switch"; i++) await page.waitForTimeout(50)
  assert.deepEqual(writes.slice(-1), ["POST /api/nodes/fixture-remote/upgrade/switch"])
  assert.ok(!writes.includes("POST /api/nodes/fixture-remote/upgrade"), "switch now does not start another upgrade")
  nodes = nodes.map(n => n.node_id === remote.node_id ? { ...n, upgrade: remote.upgrade } : n)
  await reread()
  await detail.locator('[data-node-upgrade="current"]').waitFor()
  await page.locator("[data-node-pause=active]").click()
  await page.locator("[data-node-pause=paused]").waitFor()
  assert.deepEqual(writes.slice(-1), ["POST /api/nodes/fixture-remote/pause"], "stopping new work is the owner's explicit action")
  await page.locator("[data-back]").click()
  await page.waitForFunction(() => location.hash === "#/devices/nodes")
  await row().locator("[data-node-status=paused]").getByText("已暂停接收", { exact: true }).waitFor()

  for (const [change, kind, words] of [[{ state: "active", linked: false }, "offline", "离线 · 1 分钟前"], [{ state: "pending" }, "pending", "待确认指纹"], [{ state: "active", waiting: 0, linked: true }, "online", "在线"]]) {
    nodes = nodes.map(n => n.node_id === remote.node_id ? { ...n, ...change } : n)
    await reread()
    await row().locator(`[data-node-status=${kind}]`).getByText(words, { exact: true }).waitFor()
  }
  nodes = nodes.map(n => n.node_id === remote.node_id ? { ...n, state: "revoked" } : n)
  await reread()
  await page.locator("[data-nodes-revoked] summary").getByText("已撤销配对 · 1", { exact: true }).waitFor()
  status = 503
  await reread()
  await page.locator("[data-node-error]").waitFor()
  assert.equal(await page.locator("[data-node-row=local]").count(), 1, "a failed read keeps what was read last")
  status = 200; nodes = [{ ...remote }, { ...remote, node_id: "fixture-local", local: true }]
  const beforePoll = reads
  await page.locator("[data-node-error]").waitFor({ state: "detached", timeout: 13_000 })
  assert.ok(reads > beforePoll, "visible inventory refreshes without a page reload")

  await page.goto(base + "/test/fixtures/devices.html#/devices")
  await page.locator("[data-devices-overview]").waitFor()
  await page.locator('[data-attention-row="offline"], [data-stat="nodes"]').first().waitFor()
  await page.locator('[data-stat="sessions"]').getByText("4 个在排队", { exact: true }).waitFor()
  await page.setViewportSize({ width: 390, height: 844 })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "mobile has no horizontal overflow")
  if (process.env.STEVE_TEST_SHOTS) await page.screenshot({ path: process.env.STEVE_TEST_SHOTS + "/devices-mobile.png", fullPage: true, animations: "disabled" })
  assert.deepEqual(errors, [])
  process.stdout.write("Devices: Node list, one status per Node, loading/failure, refresh, deduplication, the Node's page, its limit and pause, overview and mobile passed\n")
} finally { await browser.close() }
