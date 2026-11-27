// Vite-only fixture; external Playwright prerequisite, all API calls mocked.
// Verifies: Devices · Monitor tab (#/devices/monitor) for owners (5 sections, 15m/1h/24h windows, golden KpiCharts,
// collapsed insufficient-sample metrics, Node & plugin health matrices, mobile/dark layout, en locale), Devices ·
// Overview alert surfacing, non-owner access restriction (zero /api/system/health requests & hidden tab), and
// Settings · About summary entry on updates.html.
import assert from "node:assert/strict"
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright")
const browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE } : {}) })
const base = process.env.STEVE_TEST_WEB || "http://127.0.0.1:5198"

const NOW = Date.UTC(2026, 10, 17, 12, 0), MIN = 60_000, HOUR = 60 * MIN, GIB = 2 ** 30, HUB = "0.2.112"
const FP = "9e3f5a07c21b84d60f9a77e3b10c4d52"
const nodes = [
  {
    node_id: "local", name: "Studio Mac", fingerprint: FP, state: "active", linked: true, local: true,
    max_sessions: 32, running: 2, waiting: 0, last_seen: NOW - 10_000,
    host: { os: "macos", os_version: "26.1", arch: "aarch64", cpus: 14, mem_total: 64 * GIB },
  },
  {
    node_id: "n-build", name: "build-linux-01", fingerprint: FP, state: "active", linked: true, local: false,
    max_sessions: 8, running: 1, waiting: 0, last_seen: NOW - 20_000,
    host: { os: "linux", os_version: "24.04", arch: "x86_64", cpus: 16, mem_total: 32 * GIB },
    link: { route: "lan", via: "direct", tls: false, since: NOW - HOUR, latency_ms: 14, latency_at: NOW },
    upgrade: { build: { version: HUB, commit: "abc", target: "x" }, policy: "notify", own_policy: null, can: false, reason: "current", latest: null },
  },
]

function buildHealth(win, mode = "warned") {
  const count = win === "15m" ? 15 : win === "1h" ? 20 : 12
  const stepMs = win === "24h" ? 5 * MIN : MIN
  const windowSeconds = win === "15m" ? 900 : win === "1h" ? 3600 : 14_400 // 24h < 75% to verify short-coverage notice
  const series = Array.from({ length: count }, (_, i) => ({
    at: NOW - (count - 1 - i) * stepMs,
    http_p95: mode === "warned" ? 0.62 + 0.05 * Math.sin(i) : 0.09 + 0.01 * Math.sin(i),
    write_lock_wait_p95: i % 5 === 0 ? null : 0.018 + 0.004 * Math.cos(i),
    hub_cpu: 0.24 + 0.04 * Math.sin(i),
    hub_memory: (480 + i * 4) * 2 ** 20,
  }))
  const httpSlow = { key: "http_p95", value: mode === "warned" ? 0.72 : 0.11, unit: "seconds", max: 0.5 }
  const backupLow = { key: "backup_destination_space", value: mode === "warned" ? 200 * 2 ** 20 : 20 * GIB, unit: "bytes", min: 512 * 2 ** 20 }
  const jobDown = { key: "job_up", subject: "mail.sync", value: 0, unit: "up", min: 1 }
  const nodeDiskHigh = { key: "node_disk_writes", subject: "n-build", value: 8 * GIB, unit: "bytes_per_day", max: 5 * GIB }
  const checks = [
    httpSlow,
    { key: "write_lock_wait_p95", value: 0.02, unit: "seconds", max: 0.1 },
    { key: "cache_hit_ratio", value: null, unit: "ratio", min: 0.5 },
    { key: "sse_delivery_p95", value: 0.03, unit: "seconds", max: 0.5 },
    { key: "patch_delay_p95", value: null, unit: "seconds", max: 0.5 },
    { key: "reconcile_diff", value: 0, unit: "count", max: 0 },
    { key: "ingest_p95", value: null, unit: "seconds", max: 0.25 },
    { key: "ingest_rate", value: 1.8, unit: "per_second" },
    { key: "hub_cpu", value: 0.28, unit: "ratio", max: 0.8 },
    { key: "hub_memory", value: 512 * 2 ** 20, unit: "bytes", max: 2 * GIB },
    { key: "hub_disk_writes", value: 256 * 2 ** 20, unit: "bytes_per_day", max: 5 * GIB },
    backupLow,
    { key: "web_interaction_p75", value: null, unit: "seconds", max: 0.2 },
    { key: "web_long_tasks", value: 0.8, unit: "per_minute", max: 10 },
    { key: "job_panics", value: 0, unit: "count", max: 0 },
    { key: "jobs_down", value: mode === "warned" ? 1 : 0, unit: "count", max: 0 },
    ...(mode === "warned" ? [jobDown] : []),
    { key: "node_link", subject: "local", value: 1, unit: "up" },
    { key: "node_link_drops", subject: "local", value: 0, unit: "count", max: 3 },
    { key: "node_command_failures", subject: "local", value: null, unit: "ratio", max: 0.2 },
    { key: "node_backlog", subject: "local", value: 0, unit: "count", max: 10_000 },
    { key: "node_disk_writes", subject: "local", value: 120 * 2 ** 20, unit: "bytes_per_day", max: 5 * GIB },
    { key: "node_link", subject: "n-build", value: 1, unit: "up" },
    { key: "node_link_drops", subject: "n-build", value: 0, unit: "count", max: 3 },
    { key: "node_command_failures", subject: "n-build", value: 0.04, unit: "ratio", max: 0.2 },
    { key: "node_backlog", subject: "n-build", value: 3, unit: "count", max: 10_000 },
    mode === "warned" ? nodeDiskHigh : { key: "node_disk_writes", subject: "n-build", value: 256 * 2 ** 20, unit: "bytes_per_day", max: 5 * GIB },
    { key: "plugin_errors", subject: "lark", value: 0.02, unit: "ratio", max: 0.2 },
    { key: "plugin_restarts", subject: "lark", value: 0, unit: "count", max: 2 },
    { key: "plugin_backlog", subject: "lark", value: 0, unit: "count", max: 100 },
  ]
  const warnings = mode === "warned"
    ? [httpSlow, backupLow, { key: "jobs_down", value: 1, unit: "count", max: 0 }, jobDown, nodeDiskHigh]
    : []
  return { window: win, window_seconds: windowSeconds, sampled_at: NOW, checks, warnings, series }
}

const json = (route, body, code = 200) => route.fulfill({ status: code, contentType: "application/json", body: JSON.stringify(body) })
const errors = []

try {
  // 1. Owner flow on Devices · Monitor & Overview (zh-CN, 1440px -> 390px dark)
  {
    const ctx = await browser.newContext({ locale: "zh-CN", timezoneId: "UTC", viewport: { width: 1440, height: 960 }, reducedMotion: "reduce" })
    await ctx.clock.setFixedTime(NOW)
    await ctx.addInitScript(() => localStorage.setItem("steve-lang", "zh-CN"))
    const page = await ctx.newPage()
    page.on("pageerror", (e) => errors.push(e.message))

    let mode = "warned"
    let healthStatus = 200
    const healthCalls = []
    await page.route("**/*", async (route) => {
      const req = route.request(), u = new URL(req.url()), p = u.pathname
      if (u.origin !== base) return route.abort("blockedbyclient")
      if (!p.startsWith("/api/")) return route.continue()
      if (p === "/api/nodes") return json(route, { nodes, hub_version: HUB })
      if (p === "/api/devices") return json(route, { devices: [], runtimes: [] })
      if (p === "/api/vitals") return json(route, { nodes: {} })
      if (p === "/api/node-harnesses") return json(route, { nodes: {} })
      if (p === "/api/nodes/upgrade-default") return json(route, { policy: "notify", hub: { version: HUB, commit: "abc", target: "x" }, drain_secs: 1800, health_secs: 180 })
      if (p === "/api/node-context") return json(route, { owner_context: "off" })
      if (p === "/api/access") return json(route, { tokens: [], clients: [{ id: "c1", kind: "web", name: "Browser", platform: "macOS", version: HUB, origin: null, created: 0, last_seen: NOW, last_route: "lan", last_ip: null, revoked: null }], current: "c1", route: "lan", hub: { version: HUB } })
      if (p === "/api/system/health") {
        const win = u.searchParams.get("window") || "15m"
        healthCalls.push(win)
        if (healthStatus !== 200) return json(route, { error: "upstream unavailable" }, healthStatus)
        return json(route, buildHealth(win, mode))
      }
      return json(route, {}, 404)
    })

    // Overview shows consolidated health alert row and navigates to Monitor
    await page.goto(base + "/test/fixtures/devices.html#/devices")
    const attnRow = page.locator('[data-attention-row="system-health"]')
    await attnRow.waitFor()
    await attnRow.getByText(/系统监控：5 项指标超出阈值/).waitFor()
    await attnRow.locator("[data-attention-monitor]").click()
    await page.waitForFunction(() => location.hash === "#/devices/monitor")

    // Tab bar shows monitor count with critical badge
    const monitorSeg = page.locator('[data-seg="monitor"]')
    await monitorSeg.locator(".ui-segc__count").getByText("5", { exact: true }).waitFor()
    assert.equal(await monitorSeg.getAttribute("data-bad"), "")

    // 1. Overview & KPI strip
    const overview = page.locator("[data-monitor-overview]")
    await overview.waitFor()
    assert.equal(await overview.getAttribute("data-monitor-status"), "critical")
    assert.equal(await page.locator('[data-monitor-kpi="alerts"]').innerText().then((s) => s.includes("5")), true)

    // 2. Pinned alerts sorted critical-first (backup_destination_space & jobs_down/job_up before http_p95 & node_disk_writes)
    const warnKeys = await page.locator("[data-monitor-warnings] [data-health-warning]").evaluateAll((els) =>
      els.map((el) => [el.getAttribute("data-health-warning"), el.getAttribute("data-warning-severity")]),
    )
    assert.deepEqual(warnKeys, [
      ["backup_destination_space", "critical"],
      ["jobs_down", "critical"],
      ["job_up", "critical"],
      ["http_p95", "warning"],
      ["node_disk_writes", "warning"],
    ])
    await page.locator('[data-monitor-warnings] [data-health-warning="node_disk_writes"] button').getByText("查看 Node").waitFor()
    await page.locator('[data-monitor-warnings] [data-health-warning="backup_destination_space"] button').getByText("查看数据与备份").waitFor()

    // 3. 4 Golden signals with KpiChart SVG curves
    for (const k of ["http_p95", "write_lock_wait_p95", "hub_cpu", "hub_memory"]) {
      const card = page.locator(`[data-golden-metric="${k}"]`)
      await card.waitFor()
      assert.equal(await card.locator(".ui-kpic__lines path").count(), 1, `${k} chart draws series`)
    }

    // 4. 4 Hub subsystems: measured items visible; insufficient-sample items weakened inside collapsed <details>
    for (const id of ["request", "realtime", "resources", "clientAndJobs"]) {
      await page.locator(`[data-health-subsystem="${id}"]`).waitFor()
    }
    const reqCard = page.locator('[data-health-subsystem="request"]')
    await reqCard.locator('[data-health-check="http_p95"]').getByText("720 毫秒").waitFor()
    const ins = reqCard.locator('[data-health-insufficient="request"]')
    assert.equal(await ins.locator('[data-health-check="cache_hit_ratio"]').isVisible(), false)
    await ins.locator("summary").click()
    await ins.locator('[data-health-check="cache_hit_ratio"][data-sample-state="insufficient"]').waitFor()
    await ins.locator('[data-health-check="cache_hit_ratio"]').getByText("—").waitFor()

    // 5. Node & Plugin matrices (resolved Node names + weakened insufficient cells)
    const nodesMatrix = page.locator("[data-monitor-nodes]")
    await nodesMatrix.locator('[data-monitor-node-row="local"]').getByText("本机 Node").waitFor()
    await nodesMatrix.locator('[data-monitor-node-row="local"] [data-sample-state="insufficient"]').waitFor()
    await nodesMatrix.locator('[data-monitor-node-row="n-build"]').getByText("build-linux-01").waitFor()
    await page.locator('[data-monitor-plugins] [data-monitor-plugin-row="lark"]').waitFor()

    // Window switching (1h -> 24h) + manual refresh + refresh error keeps last data
    await page.locator('[data-monitor-window] [data-window="1h"]').click()
    await page.waitForFunction(() => document.querySelector("[data-monitor-coverage]")?.textContent?.includes("1 小时"))
    await page.locator('[data-monitor-window] [data-window="24h"]').click()
    await page.waitForFunction(() => document.querySelector("[data-monitor-coverage]")?.textContent?.includes("4 小时"))
    assert.ok(healthCalls.includes("1h") && healthCalls.includes("24h"))

    const callsBeforeRefresh = healthCalls.length
    healthStatus = 503
    await page.locator("[data-monitor-refresh]").click()
    await page.locator("[data-monitor-error]").waitFor()
    assert.equal(healthCalls.length, callsBeforeRefresh + 1)
    assert.equal(await page.locator("[data-monitor-overview]").isVisible(), true)

    // Healthy state on 15m removes pinned warnings, tab badge, and overview attention row
    healthStatus = 200
    mode = "healthy"
    await page.locator('[data-monitor-window] [data-window="15m"]').click()
    await page.locator('[data-monitor-overview][data-monitor-status="healthy"]').waitFor()
    assert.equal(await page.locator("[data-monitor-warnings]").count(), 0)
    assert.equal(await monitorSeg.locator(".ui-segc__count").count(), 0)

    // 390px mobile + dark mode: zero horizontal overflow and mobile node cards appear
    await page.setViewportSize({ width: 390, height: 844 })
    await page.evaluate(() => document.documentElement.classList.add("dark"))
    await page.locator('[data-monitor-node-card="local"]').waitFor()
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "monitor mobile has no horizontal overflow")

    // Overview when healthy has no system-health row
    await page.locator('[data-seg="overview"]').click()
    await page.locator("[data-devices-overview]").waitFor()
    assert.equal(await page.locator('[data-attention-row="system-health"]').count(), 0)

    await ctx.close()
  }

  // 2. Non-owner access gate: monitor tab hidden, direct hash falls back to overview, zero /api/system/health calls
  {
    const ctx = await browser.newContext({ locale: "zh-CN", viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
    const page = await ctx.newPage()
    page.on("pageerror", (e) => errors.push(e.message))
    let healthRequests = 0
    await page.route("**/*", async (route) => {
      const u = new URL(route.request().url()), p = u.pathname
      if (u.origin !== base) return route.abort("blockedbyclient")
      if (!p.startsWith("/api/")) return route.continue()
      if (p === "/api/system/health") { healthRequests++; return json(route, { error: "owner only" }, 403) }
      if (p === "/api/nodes") return json(route, { nodes: [nodes[0]], hub_version: HUB })
      if (p === "/api/devices") return json(route, { devices: [], runtimes: [] })
      if (p === "/api/vitals") return json(route, { nodes: {} })
      if (p === "/api/node-harnesses") return json(route, { nodes: {} })
      if (p === "/api/nodes/upgrade-default") return json(route, { policy: "notify", hub: { version: HUB, commit: "abc", target: "x" }, drain_secs: 1800, health_secs: 180 })
      if (p === "/api/node-context") return json(route, { owner_context: "off" })
      if (p === "/api/access") return json(route, { tokens: [], clients: [], current: "c1", route: "lan", hub: { version: HUB } })
      return json(route, {}, 404)
    })
    await page.goto(base + "/test/fixtures/devices.html?member#/devices/monitor")
    await page.locator("[data-devices-overview]").waitFor()
    assert.equal(await page.locator('[data-seg="monitor"]').count(), 0)
    assert.equal(await page.locator("[data-monitor-overview]").count(), 0)
    assert.equal(healthRequests, 0, "non-owner never requests /api/system/health")
    await ctx.close()
  }

  // 3. Settings · About summary entry on updates.html (healthy, warned, and 403)
  {
    const ctx = await browser.newContext({ locale: "zh-CN", viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
    const page = await ctx.newPage()
    page.on("pageerror", (e) => errors.push(e.message))
    let healthMode = "healthy"
    await page.route("**/*", async (route) => {
      const u = new URL(route.request().url()), p = u.pathname
      if (u.origin !== base) return route.abort("blockedbyclient")
      if (!p.startsWith("/api/")) return route.continue()
      if (p === "/api/updates") return json(route, { repository: "Infranite/steve", current: { hub: HUB, web: HUB }, capability: { managed: false, can_execute: false, reason: "source checkout" }, check: null, operation: null })
      if (p === "/api/system/health") {
        if (healthMode === "forbidden") return json(route, { error: "owner only" }, 403)
        return json(route, buildHealth("15m", healthMode))
      }
      return json(route, {}, 404)
    })
    await page.goto(base + "/test/fixtures/updates.html")
    await page.locator("[data-hub-health] [data-health-summary-row]").waitFor()
    assert.equal(await page.locator("[data-health-group]").count(), 0, "old detailed health groups are removed from Settings · About")
    await page.locator('[data-health-summary-badge="healthy"]').getByText("正常").waitFor()
    await page.locator("[data-health-summary-row]").click()
    await page.waitForFunction(() => location.hash === "#/devices/monitor")

    healthMode = "warned"
    await page.goto(base + "/test/fixtures/updates.html")
    await page.locator('[data-health-summary-badge="critical"]').getByText("严重").waitFor()
    await page.locator("[data-health-summary-row]").getByText(/等 5 项，点此前往监控页排查/).waitFor()

    healthMode = "forbidden"
    await page.goto(base + "/test/fixtures/updates.html")
    await page.locator("[data-version-info]").waitFor()
    assert.equal(await page.locator("[data-hub-health]").count(), 0)
    await ctx.close()
  }

  // 4. English locale check
  {
    const ctx = await browser.newContext({ locale: "en-US", timezoneId: "UTC", viewport: { width: 1280, height: 900 }, reducedMotion: "reduce" })
    await ctx.clock.setFixedTime(NOW)
    await ctx.addInitScript(() => localStorage.setItem("steve-lang", "en"))
    const page = await ctx.newPage()
    page.on("pageerror", (e) => errors.push(e.message))
    await page.route("**/*", async (route) => {
      const u = new URL(route.request().url()), p = u.pathname
      if (u.origin !== base) return route.abort("blockedbyclient")
      if (!p.startsWith("/api/")) return route.continue()
      if (p === "/api/nodes") return json(route, { nodes, hub_version: HUB })
      if (p === "/api/devices") return json(route, { devices: [], runtimes: [] })
      if (p === "/api/vitals") return json(route, { nodes: {} })
      if (p === "/api/node-harnesses") return json(route, { nodes: {} })
      if (p === "/api/nodes/upgrade-default") return json(route, { policy: "notify", hub: { version: HUB, commit: "abc", target: "x" }, drain_secs: 1800, health_secs: 180 })
      if (p === "/api/node-context") return json(route, { owner_context: "off" })
      if (p === "/api/access") return json(route, { tokens: [], clients: [], current: "c1", route: "lan", hub: { version: HUB } })
      if (p === "/api/system/health") return json(route, buildHealth(u.searchParams.get("window") || "15m", "healthy"))
      return json(route, {}, 404)
    })
    await page.goto(base + "/test/fixtures/devices.html#/devices/monitor")
    await page.locator('[data-monitor-overview][data-monitor-status="healthy"]').waitFor()
    await page.getByText("All systems healthy").waitFor()
    const body = await page.locator("body").innerText()
    assert.match(body, /All systems healthy/)
    assert.match(body, /Golden signals/)
    assert.match(body, /Hub subsystems/)
    assert.match(body, /Node health matrix/)
    assert.match(body, /Plugin health matrix/)
    assert.equal(/\b(?:devices|shell):[a-zA-Z][\w.]*\b/.test(body), false, "no raw i18n keys in English monitor")
    await ctx.close()
  }

  assert.deepEqual(errors, [])
  console.log("Devices monitor: owner flow, 15m/1h/24h windows, golden charts, collapsed insufficient samples, overview alert, non-owner gate, About summary, mobile/dark, and en locale passed")
} finally {
  await browser.close()
}
