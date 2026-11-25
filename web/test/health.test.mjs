import assert from "node:assert/strict"
import { test } from "node:test"

const h = await import("../src/health.ts")
const perf = await import("../src/perf.ts")

test("health groups the Hub's figures, then each Node's, job's and plugin's, and words what is past its limit", () => {
  const slow = { key: "http_p95", value: 0.95, unit: "seconds", max: 0.5 }
  const disk = { key: "node_disk_writes", subject: "mini", value: 10 * 2 ** 30, unit: "bytes_per_day", max: 5 * 2 ** 30 }
  const jobDown = { key: "job_up", subject: "mail.sync", value: 0, unit: "up", min: 1 }
  const report = {
    window_seconds: 900,
    checks: [slow, { key: "cache_hit_ratio", value: null, unit: "ratio", min: 0.5 }, jobDown, disk, { key: "plugin_restarts", subject: "lark", value: 0, unit: "count", max: 2 }, { key: "from_a_newer_hub", value: 1, unit: "count" }],
    warnings: [slow, jobDown, disk, { key: "from_a_newer_hub", value: 1, unit: "count" }],
  }
  assert.deepEqual(h.grouped(report).map((g) => [g.title, g.checks.map((c) => c.key)]), [
    ["Hub", ["http_p95", "cache_hit_ratio"]], ["后台任务 mail.sync", ["job_up"]], ["节点 mini", ["node_disk_writes"]], ["插件 lark", ["plugin_restarts"]],
  ])
  assert.deepEqual(h.warnings(report).map(h.warningText), [
    "请求变慢了：p95 为 950 毫秒，超过 500 毫秒。",
    "后台任务 mail.sync 在多次崩溃后停摆。",
    "节点 mini 的磁盘写入折合 10.0 GB/天，超过 5.0 GB/天。",
  ])
  assert.equal(h.valueText(report.checks[1]), "样本不足")
  assert.equal(h.limitText(report.checks[1]), "下限 50%")
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
  assert.equal(perf.pageOf("#/items"), "items")
  assert.equal(perf.pageOf("#/items/ST-42"), "item")
  assert.equal(perf.pageOf("#/sessions"), "sessions")
  assert.equal(perf.pageOf("#/sessions/s1"), "session")
  assert.equal(perf.pageOf("#/calendar"), "calendar")
  assert.equal(perf.pageOf("#/mail"), "mail")
  assert.equal(perf.pageOf("#/contacts/me"), "me")
  assert.equal(perf.pageOf("#/contacts"), "contacts")
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
