import assert from "node:assert/strict"
import { test } from "node:test"

const h = await import("../src/health.ts")
const perf = await import("../src/perf.ts")

test("health partitions subsystems, sorts warnings critical-first, and builds Node/plugin matrices", () => {
  const slow = { key: "http_p95", value: 0.95, unit: "seconds", max: 0.5 }
  const lockOk = { key: "write_lock_wait_p95", value: 0.02, unit: "seconds", max: 0.1 }
  const cacheMiss = { key: "cache_hit_ratio", value: null, unit: "ratio", min: 0.5 }
  const sseOk = { key: "sse_delivery_p95", value: 0.04, unit: "seconds", max: 0.5 }
  const cpuOk = { key: "hub_cpu", value: 0.32, unit: "ratio", max: 0.8 }
  const memOk = { key: "hub_memory", value: 512 * 2 ** 20, unit: "bytes", max: 2 * 2 ** 30 }
  const jobDown = { key: "job_up", subject: "mail.sync", value: 0, unit: "up", min: 1 }
  const disk = { key: "node_disk_writes", subject: "mini", value: 10 * 2 ** 30, unit: "bytes_per_day", max: 5 * 2 ** 30 }
  const localLink = { key: "node_link", subject: "local", value: 1, unit: "up" }
  const miniLink = { key: "node_link", subject: "mini", value: 1, unit: "up" }
  const report = {
    window: "15m",
    window_seconds: 900,
    sampled_at: 1_700_000_000_000,
    checks: [
      slow, lockOk, cacheMiss, sseOk, cpuOk, memOk, jobDown,
      localLink, miniLink, disk,
      { key: "plugin_errors", subject: "lark", value: 0.05, unit: "ratio", max: 0.2 },
      { key: "plugin_restarts", subject: "lark", value: 0, unit: "count", max: 2 },
      { key: "plugin_backlog", subject: "discord", value: null, unit: "count", max: 100 },
      { key: "from_a_newer_hub", value: 1, unit: "count" },
    ],
    warnings: [slow, jobDown, disk, { key: "from_a_newer_hub", value: 1, unit: "count" }],
    series: [
      { at: 1_699_999_940_000, http_p95: 0.2, write_lock_wait_p95: null, hub_cpu: 0.2, hub_memory: 400 * 2 ** 20 },
      { at: 1_700_000_000_000, http_p95: 0.95, write_lock_wait_p95: 0.02, hub_cpu: 0.32, hub_memory: 512 * 2 ** 20 },
    ],
  }

  assert.equal(h.overallStatus(report), "critical")
  assert.deepEqual(h.sortedWarnings(report).map((w) => [w.key, h.severityOf(w)]), [
    ["job_up", "critical"],
    ["http_p95", "warning"],
    ["node_disk_writes", "warning"],
  ])
  assert.deepEqual(h.warnings(report).map(h.warningText), [
    "请求变慢了：p95 为 950 毫秒，超过 500 毫秒。",
    "后台任务 mail.sync 在多次崩溃后停摆。",
    "节点 mini 的磁盘写入折合 10.0 GB/天，超过 5.0 GB/天。",
  ])

  assert.equal(h.valueText(cacheMiss), "样本不足")
  assert.equal(h.displayValue(cacheMiss), "—")
  assert.equal(h.displayValue(slow), "950 毫秒")
  assert.equal(h.limitText(cacheMiss), "下限 50%")
  assert.ok(Math.abs((h.utilization(cpuOk) ?? 0) - 0.4) < 1e-12)
  assert.equal(h.utilization(cacheMiss), null)
  assert.equal(h.windowCoverageShort(report), false)
  assert.equal(h.windowCoverageShort({ ...report, window: "1h", window_seconds: 1200 }), true)
  assert.deepEqual(h.checkCounts(report), { total: 13, measured: 11, noData: 2, warnings: 3 })

  const groups = h.subsystemGroups(report)
  assert.deepEqual(groups.map((g) => g.id), ["request", "realtime", "resources", "clientAndJobs"])
  assert.deepEqual(groups[0].measured.map((c) => c.key), ["http_p95", "write_lock_wait_p95"])
  assert.deepEqual(groups[0].noData.map((c) => c.key), ["cache_hit_ratio"])
  assert.deepEqual(groups[3].measured.map((c) => [c.key, c.subject]), [["job_up", "mail.sync"]])

  const nodes = h.nodeHealthMatrix(report)
  assert.deepEqual(nodes.map((n) => n.node), ["mini", "local"])
  assert.equal(nodes[0].diskWrites?.value, 10 * 2 ** 30)
  assert.equal(nodes[0].warnings.length, 1)

  const plugins = h.pluginHealthMatrix(report)
  assert.deepEqual(plugins.map((p) => p.plugin), ["discord", "lark"])
  assert.equal(plugins[0].backlog?.value, null)

  const httpTrend = h.goldenSeries(report, "http_p95")
  assert.equal(httpTrend.warned, true)
  assert.deepEqual(httpTrend.values, [200, 950])
  assert.deepEqual(h.goldenSeries(report, "write_lock_wait_p95").values, [null, 20])
  assert.deepEqual(h.goldenSeries(report, "hub_cpu").values, [20, 32])
})

test("the web reporter counts an interaction once at its slowest and sends nothing when nothing happened", () => {
  const c = perf.collector()
  assert.equal(c.take(), null)
  c.longTask(80.4)
  c.interaction(7, 120)
  c.interaction(7, 260)
  c.interaction(0, 999)
  c.interaction(8, 90_000)
  assert.deepEqual(c.take(), { long_tasks: [80], interactions: [260, 60_000] })
  assert.equal(c.take(), null)
})

test("pageOf maps every nav destination and detail route into the bounded WEB_PAGES set", () => {
  assert.equal(perf.pageOf(""), "chat")
  assert.equal(perf.pageOf("#/"), "chat")
  assert.equal(perf.pageOf("#/c/thread-1"), "chat")
  assert.equal(perf.pageOf("#/feed"), "feed")
  assert.equal(perf.pageOf("#/inbox"), "inbox")
  assert.equal(perf.pageOf("#/items"), "items")
  assert.equal(perf.pageOf("#/items/ST-42"), "item")
  assert.equal(perf.pageOf("#/projects"), "projects")
  assert.equal(perf.pageOf("#/projects/p1"), "projects")
  assert.equal(perf.pageOf("#/sessions"), "sessions")
  assert.equal(perf.pageOf("#/sessions/s1"), "session")
  assert.equal(perf.pageOf("#/calendar"), "calendar")
  assert.equal(perf.pageOf("#/todos"), "todos")
  assert.equal(perf.pageOf("#/channels"), "channels")
  assert.equal(perf.pageOf("#/mail"), "mail")
  assert.equal(perf.pageOf("#/contacts/me"), "me")
  assert.equal(perf.pageOf("#/contacts"), "contacts")
  assert.equal(perf.pageOf("#/orgs/o1"), "contacts")
  assert.equal(perf.pageOf("#/teams/t1"), "contacts")
  assert.equal(perf.pageOf("#/knowledge"), "knowledge")
  assert.equal(perf.pageOf("#/integrations"), "integrations")
  assert.equal(perf.pageOf("#/bots"), "bots")
  assert.equal(perf.pageOf("#/auto/rules"), "auto")
  assert.equal(perf.pageOf("#/capabilities"), "capabilities")
  assert.equal(perf.pageOf("#/signals"), "signals")
  assert.equal(perf.pageOf("#/stats"), "stats")
  assert.equal(perf.pageOf("#/devices"), "devices")
  assert.equal(perf.pageOf("#/devices/monitor"), "devices")
  assert.equal(perf.pageOf("#/hub"), "devices")
  assert.equal(perf.pageOf("#/accounts"), "accounts")
  assert.equal(perf.pageOf("#/vault"), "vault")
  assert.equal(perf.pageOf("#/settings/models"), "settings")
  assert.equal(perf.pageOf("#/more"), "settings")
  assert.equal(perf.pageOf("#/design"), "settings")
  assert.equal(perf.pageOf("#/design/components"), "settings")
  assert.equal(perf.pageOf("#/publications"), "publications")
  assert.equal(perf.pageOf("#/d/del_1"), "publications")
  assert.equal(perf.pageOf("#/github"), "github")
  assert.equal(perf.pageOf("#/unknown-page"), "other")
})

test("web errors and SSE reconnects are buffered and flushed through web-perf", async () => {
  const sent = []
  perf.resetNavTracker(async (report) => { sent.push(report) })
  perf.recordWebError("error")
  perf.recordWebError("error")
  perf.recordWebError("unhandledrejection")
  perf.recordSseReconnect("stale")
  perf.recordSseReconnect("wake")
  await perf.flushNavOpens()
  assert.equal(sent.length, 1)
  assert.deepEqual(sent[0].errors, { error: 2, unhandledrejection: 1 })
  assert.deepEqual(sent[0].sse_reconnects, { stale: 1, wake: 1 })
  await perf.flushNavOpens()
  assert.equal(sent.length, 1, "flushing again sends nothing once drained")
  perf.resetNavTracker()
})
