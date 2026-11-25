// Browser smoke test for the tabbed Node detail page: verifies tab switching, URL hash persistence
// across full page reload, shared actions between Overview and target tabs, and responsive layout.
// Fixture shapes match web/src/gen/* (NodeVitalsDay, NodeOffline, FileAccessReport, HarnessDegradedReason).
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")

const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"
const browser = await chromium.launch({
  headless: true,
  ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}),
})
const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1360, height: 960 } })
page.setDefaultTimeout(12_000)

const errors = []
const writes = []
page.on("pageerror", (e) => errors.push(e.message))

const NOW = Date.UTC(2026, 10, 25, 10, 0)
const MIN = 60_000, HOUR = 60 * MIN, GIB = 2 ** 30, HUB = "0.2.112"
const FP = "9e3f5a07c21b84d60f9a77e3b10c4d52"

let nodes = [
  {
    node_id: "n-auth",
    name: "Design Mac mini",
    fingerprint: FP,
    state: "active",
    linked: true,
    local: false,
    max_sessions: 6,
    running: 1,
    waiting: 0,
    last_seen: NOW - 15_000,
    offline: null,
    link: { route: "lan", via: "direct", tls: false, since: NOW - HOUR, latency_ms: 14, latency_at: NOW - 5_000 },
    host: {
      hostname: "design-mini.local",
      os: "macos",
      os_name: "macOS",
      os_version: "26.1",
      arch: "aarch64",
      cpus: 12,
      mem_total: 36 * GIB,
      file_access: { status: "missing", locations: [{ location: "documents", status: "missing" }] },
    },
    upgrade: {
      build: { version: "0.2.109", commit: "abc12345", target: "aarch64-apple-darwin" },
      policy: "notify",
      own_policy: null,
      can: true,
      reason: "outdated",
      latest: null,
    },
    removal: { reason: "ok", latest: null, command: "steve node stop && steve node uninstall && rm -rf ~/.steve-node", grace_secs: 60 },
  },
  {
    node_id: "n-offline",
    name: "studio-imac",
    fingerprint: FP,
    state: "active",
    linked: false,
    local: false,
    max_sessions: 4,
    running: 0,
    waiting: 1,
    last_seen: NOW - 3 * HOUR,
    offline: { reason: "unreachable", since: NOW - 3 * HOUR, duration_ms: 3 * HOUR },
    link_error: { at: NOW - 3 * HOUR, error: "connection refused: tcp connect" },
    link: { route: "lan", via: "direct", tls: false, since: 0, latency_ms: null, latency_at: null },
    host: { hostname: "imac.local", os: "macos", os_version: "15.6", arch: "x86_64", cpus: 8, mem_total: 16 * GIB },
    upgrade: { build: { version: "0.2.109", commit: "def67890", target: "x86_64-apple-darwin" }, policy: "notify", own_policy: null, can: false, reason: "unknown", latest: null },
  },
]

// Matches web/src/gen/VitalsResponse.ts & NodeVitalsDay.ts
const vitals = {
  nodes: {
    "n-auth": {
      latest: {
        at: NOW - 20_000,
        sampled_at: NOW - 21_000,
        cpus: 12,
        load: [3.6, 3.2, 2.8],
        mem_total: 36 * GIB,
        mem_available: 18 * GIB,
        network: { rx_bytes_per_second: 520_000, tx_bytes_per_second: 110_000 },
        roots: [
          { root: "/work", display: "~/Code", free: 212 * GIB, total: 994 * GIB },
          { root: "/data", display: "/mnt/data", free: 40 * GIB, total: 500 * GIB },
        ],
        sessions: 1,
        rtt_ms: 14,
      },
      series: [
        { at: NOW - 2 * MIN, load: 0.25, mem: 0.5, rtt: 15, network: { rx_bytes_per_second: 480_000, tx_bytes_per_second: 90_000 } },
        { at: NOW - MIN, load: 0.3, mem: 0.5, rtt: 14, network: { rx_bytes_per_second: 520_000, tx_bytes_per_second: 110_000 } },
      ],
      gaps: [],
      sampling: { interval_ms: 60_000, stale_after_ms: 180_000, now: NOW },
      tokens: { per_minute: 42_000, today: 8_400_000 },
    },
  },
}

const harnesses = {
  "n-auth": {
    state: "reported",
    checked_at: NOW - 30_000,
    attempted_at: NOW - 30_000,
    stale: false,
    refreshing: false,
    error: null,
    harnesses: [
      { id: "auth-adapter", name: "Auth Adapter", available: false, version: "1.2.0", adapter_version: "0.4.0", degraded_reason: "authentication_required", models: [], error: null, setup: null },
      { id: "ready-adapter", name: "Ready Adapter", available: true, version: "2.0.0", adapter_version: "0.9.0", degraded_reason: null, models: ["fixture/model"], error: null, setup: null },
    ],
  },
}

const json = (route, body, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })

await page.route("**/*", async (route) => {
  const req = route.request(), url = new URL(req.url()), p = url.pathname
  if (url.origin !== base) return route.abort("blockedbyclient")
  if (!p.startsWith("/api/")) return route.continue()
  if (req.method() !== "GET") writes.push(`${req.method()} ${p}`)
  if (p === "/api/nodes") return json(route, { nodes })
  if (p === "/api/devices") return json(route, { devices: [], runtimes: [] })
  if (p === "/api/vitals") return json(route, vitals)
  if (p === "/api/node-harnesses") return json(route, { nodes: harnesses })
  if (p === "/api/node-harnesses/n-auth") return json(route, { node_id: "n-auth", report: harnesses["n-auth"] })
  if (p.startsWith("/api/node-harness-updates/")) return json(route, { report: { node: "n-auth", checking: false, harnesses: [] }, runs: {}, retryable_unknown: [], auto_upgrade: { enabled: false, harnesses: {} }, at: NOW, error: null, code: null })
  if (p === "/api/nodes/n-auth/file-access" && req.method() === "POST") {
    const report = { status: "granted", locations: [{ location: "documents", status: "granted" }] }
    nodes = nodes.map((n) => n.node_id === "n-auth" ? { ...n, host: { ...n.host, file_access: report } } : n)
    return json(route, report)
  }
  if (p === "/api/nodes/n-auth/upgrade" && req.method() === "POST") {
    const latest = { operation_id: "nup_smoke", version: HUB, from_version: "0.2.109", phase: "draining", error: null, requested_by: "owner", force: false, created: NOW, updated: NOW }
    nodes = nodes.map((n) => n.node_id === "n-auth" ? { ...n, upgrade: { ...n.upgrade, can: false, reason: "busy", latest } } : n)
    return json(route, { operation: "nup_smoke" }, 202)
  }
  if (p === "/api/nodes/upgrade-default") return json(route, { policy: "notify", hub: { version: HUB, commit: "abc", target: "x" }, drain_secs: 1800, health_secs: 180 })
  if (p === "/api/node-context") return json(route, { owner_context: "off" })
  if (p === "/api/node-tags") return json(route, { tags: { "n-auth": ["mac"] } })
  if (p === "/api/access") return json(route, { tokens: [], clients: [], current: null })
  return json(route, {}, 404)
})

try {
  // 1. Multi-Node list stays separate from single-Node detail
  await page.goto(base + "/test/fixtures/devices.html?strict#/devices/nodes")
  await page.locator('[data-node-row="n-auth"]').click()
  await page.waitForFunction(() => location.hash === "#/devices/nodes/n-auth")
  await page.locator('[data-node-panel="overview"]').waitFor()

  // 2. Needs-attention actions pinned at top of Overview; internal fingerprint/raw state NOT exposed here
  await page.locator('[data-node-actions]').waitFor()
  await page.locator('[data-node-action="fileAccess"]').waitFor()
  await page.locator('[data-node-action="harnessAuth"]').waitFor()
  await page.locator('[data-node-action="upgradeOutdated"]').waitFor()
  assert.equal(await page.locator("[data-node-fingerprint]").count(), 0, "fingerprint is tucked in Diagnostics")
  assert.equal(await page.locator("[data-node-raw-state]").count(), 0, "raw state is tucked in Diagnostics")

  // 3. Shared action from Overview updates both Overview and the Access tab via the single action handler
  await page.locator('[data-node-action="fileAccess"] [data-node-action-run="recheckFileAccess"]').click()
  await page.locator('[data-node-action="fileAccess"]').waitFor({ state: "detached" })
  assert.deepEqual(writes, ["POST /api/nodes/n-auth/file-access"])

  // 4. Tab switching updates URL hash cleanly and renders the expected panel
  const expectedPanels = [
    ["harness", "[data-node-harnesses='n-auth']"],
    ["work", "[data-node-resources]"],
    ["access", "[data-node-file-access='granted']"],
    ["version", "[data-node-upgrade='outdated']"],
    ["diagnostics", "[data-node-diagnostics]"],
    ["overview", "[data-node-figures]"],
  ]
  for (const [tab, marker] of expectedPanels) {
    await page.locator(`[data-node-tab="${tab}"]`).click()
    const wantHash = tab === "overview" ? "#/devices/nodes/n-auth" : `#/devices/nodes/n-auth/${tab}`
    await page.waitForFunction((h) => location.hash === h, wantHash)
    await page.locator(`[data-node-panel="${tab}"]`).waitFor()
    await page.locator(marker).waitFor()
  }

  // 5. URL tab state survives a full reload and direct deep link
  await page.locator('[data-node-tab="diagnostics"]').click()
  await page.waitForFunction(() => location.hash === "#/devices/nodes/n-auth/diagnostics")
  await page.reload()
  await page.locator('[data-node-panel="diagnostics"]').waitFor()
  assert.equal(await page.evaluate(() => location.hash), "#/devices/nodes/n-auth/diagnostics")
  // Raw JSON stays collapsed until explicitly opened
  assert.equal(await page.locator('[data-node-raw-state] details').evaluate((el) => el.open), false)
  await page.locator("[data-node-raw-toggle]").click()
  await page.locator("[data-node-raw-state] [data-slot=code-block]").waitFor()
  await page.locator(`[data-node-fingerprint="${FP}"]`).waitFor()

  // Direct link to Version tab + unknown tab fallback to overview
  await page.goto(base + "/test/fixtures/devices.html?strict#/devices/nodes/n-auth/version")
  await page.locator('[data-node-panel="version"]').waitFor()
  await page.goto(base + "/test/fixtures/devices.html?strict#/devices/nodes/n-auth/not-a-tab")
  await page.locator('[data-node-panel="overview"]').waitFor()

  // 6. Offline Node puts human reason on Overview and raw link error in Diagnostics
  await page.goto(base + "/test/fixtures/devices.html?strict#/devices/nodes/n-offline")
  await page.locator('[data-node-offline-reason="unreachable"]').waitFor()
  assert.equal(await page.locator("[data-node-link-error]").count(), 0, "raw link error stays off Overview")
  await page.locator('[data-node-action="offline"] [data-node-action-goto="diagnostics"]').click()
  await page.waitForFunction(() => location.hash === "#/devices/nodes/n-offline/diagnostics")
  await page.locator("[data-node-link-error]").getByText("connection refused: tcp connect").waitFor()

  // 7. Mobile viewport fits without horizontal page overflow
  await page.setViewportSize({ width: 390, height: 844 })
  for (const tab of ["overview", "harness", "work", "access", "version", "diagnostics"]) {
    await page.locator(`[data-node-tab="${tab}"]`).click()
    await page.locator(`[data-node-panel="${tab}"]`).waitFor()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `tab ${tab} fits 390px`)
  }

  assert.deepEqual(errors, [])
  process.stdout.write("Node tabs: tab switching, URL reload persistence, shared actions, diagnostics disclosure and mobile layout passed\n")
} finally {
  await browser.close()
}
