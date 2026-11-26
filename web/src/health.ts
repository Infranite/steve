/* Devices · Monitor (#/devices/monitor) and Settings · About summary: data model, severity ranking,
 * subsystem grouping, Node/plugin health matrices, and golden-signal series formatting over `/api/system/health`. */
/* relative .ts imports so node --test loads this file without a bundler */
import { t } from "./i18n/index.ts"
import { msDur } from "./i18n/format.ts"
import { formatBytes } from "./devices-install.ts"

export const KEYS = [
  "http_p95", "write_lock_wait_p95", "sse_delivery_p95", "patch_delay_p95", "reconcile_diff", "ingest_p95", "ingest_rate", "cache_hit_ratio",
  "hub_cpu", "hub_memory", "hub_disk_writes", "backup_destination_space", "web_interaction_p75", "web_long_tasks",
  "job_panics", "jobs_down", "job_up",
  "node_link", "node_link_drops", "node_command_failures", "node_backlog", "node_disk_writes",
  "plugin_errors", "plugin_restarts", "plugin_backlog",
] as const
export type HealthKey = (typeof KEYS)[number]
export type HealthCheck = { key: HealthKey; subject?: string; value: number | null; unit: string; max?: number; min?: number }

export const HEALTH_WINDOWS = ["15m", "1h", "24h"] as const
export type HealthWindow = (typeof HEALTH_WINDOWS)[number]
export const WINDOW_TARGET_SECONDS: Record<HealthWindow, number> = { "15m": 900, "1h": 3600, "24h": 86_400 }

export type HealthSeriesPoint = {
  at: number
  http_p95: number | null
  write_lock_wait_p95: number | null
  hub_cpu: number | null
  hub_memory: number | null
}

export type HubHealth = {
  window?: HealthWindow
  window_seconds: number
  sampled_at?: number
  checks: HealthCheck[]
  warnings: HealthCheck[]
  series?: HealthSeriesPoint[]
}

export type HealthSeverity = "critical" | "warning"
export type HealthOverallStatus = "healthy" | "warning" | "critical"

export const GOLDEN_KEYS = ["http_p95", "write_lock_wait_p95", "hub_cpu", "hub_memory"] as const
export type GoldenKey = (typeof GOLDEN_KEYS)[number]

export const SUBSYSTEM_IDS = ["request", "realtime", "resources", "clientAndJobs"] as const
export type SubsystemId = (typeof SUBSYSTEM_IDS)[number]

const SUBSYSTEM_KEYS: Record<SubsystemId, readonly HealthKey[]> = {
  request: ["http_p95", "write_lock_wait_p95", "cache_hit_ratio"],
  realtime: ["sse_delivery_p95", "patch_delay_p95", "reconcile_diff", "ingest_p95", "ingest_rate"],
  resources: ["hub_cpu", "hub_memory", "hub_disk_writes", "backup_destination_space"],
  clientAndJobs: ["web_interaction_p75", "web_long_tasks", "job_panics", "jobs_down", "job_up"],
}

const CRITICAL_KEYS = new Set<HealthKey>(["job_up", "jobs_down", "job_panics", "node_link", "backup_destination_space"])
const COVERAGE_SHORT_RATIO = 0.75

/** A figure in its unit: "240 ms", "38%", "10.4 GB/天", "3.2/秒" */
export function shown(unit: string, v: number): string {
  switch (unit) {
    case "seconds": return msDur(v * 1000)
    case "ratio": return `${v < 0.1 && v > 0 ? (v * 100).toFixed(1) : Math.round(v * 100)}%`
    case "bytes": return formatBytes(v)
    case "bytes_per_day": return t("shell:health.perDay", { amount: formatBytes(v) })
    case "per_second": return t("shell:health.perSecond", { n: v.toFixed(1) })
    case "per_minute": return t("shell:health.perMinute", { n: v.toFixed(1) })
    case "up": return v >= 1 ? t("shell:health.up") : t("shell:health.down")
    default: return String(Math.round(v))
  }
}

const known = (c: HealthCheck) => (KEYS as readonly string[]).includes(c.key)
const limitOf = (c: HealthCheck) => c.max ?? c.min

export const checkId = (c: HealthCheck): string => (c.subject ? `${c.key}:${c.subject}` : c.key)

export const severityOf = (c: HealthCheck): HealthSeverity => (CRITICAL_KEYS.has(c.key) ? "critical" : "warning")

/** "上限 500 ms" / "下限 50%", or nothing for a figure shown only for information */
export function limitText(c: HealthCheck): string | null {
  if (c.max != null) return t("shell:health.max", { limit: shown(c.unit, c.max) })
  if (c.min != null) return t("shell:health.min", { limit: shown(c.unit, c.min) })
  return null
}

/** The figure, or that the window had too few samples to say */
export const valueText = (c: HealthCheck) => (c.value == null ? t("shell:health.noData") : shown(c.unit, c.value))

/** Weakened inline display value ("—" when samples are insufficient; full explanation stays in tooltip/details). */
export const displayValue = (c: HealthCheck) => (c.value == null ? "—" : shown(c.unit, c.value))

/** Why a figure past its limit matters, in a sentence */
export const warningText = (c: HealthCheck) =>
  t(`shell:health.warn.${c.key}`, { subject: c.subject ?? "", value: valueText(c), limit: shown(c.unit, limitOf(c) ?? 0) })

export const label = (c: HealthCheck) => t(`shell:health.key.${c.key}`)

export function scopeLabel(c: HealthCheck): string {
  if (c.subject == null) return t("shell:health.hub")
  if (c.key.startsWith("node_")) return t("shell:health.node", { name: c.subject })
  if (c.key.startsWith("job_")) return t("shell:health.job", { name: c.subject })
  return t("shell:health.plugin", { name: c.subject })
}

/** Utilization ratio (0..1) against an upper limit (`max`), or null when unbounded or unmeasured. */
export function utilization(c: HealthCheck): number | null {
  if (c.value == null || c.max == null || c.max <= 0) return null
  return Math.max(0, Math.min(1, c.value / c.max))
}

export const warnings = (h: HubHealth) => h.warnings.filter(known)

/** Known warnings sorted critical-first, preserving Hub order within each severity tier. */
export function sortedWarnings(h: HubHealth): HealthCheck[] {
  const rank = (c: HealthCheck) => (severityOf(c) === "critical" ? 0 : 1)
  return warnings(h)
    .map((c, index) => ({ c, index }))
    .sort((a, b) => rank(a.c) - rank(b.c) || a.index - b.index)
    .map((item) => item.c)
}

export function overallStatus(h: HubHealth): HealthOverallStatus {
  const past = sortedWarnings(h)
  if (past.some((c) => severityOf(c) === "critical")) return "critical"
  if (past.length > 0) return "warning"
  return "healthy"
}

export function windowCoverageShort(h: HubHealth): boolean {
  const w: HealthWindow = h.window ?? "15m"
  return h.window_seconds < WINDOW_TARGET_SECONDS[w] * COVERAGE_SHORT_RATIO
}

export function checkCounts(h: HubHealth): { total: number; measured: number; noData: number; warnings: number } {
  const all = h.checks.filter(known)
  const measured = all.filter((c) => c.value != null).length
  return { total: all.length, measured, noData: all.length - measured, warnings: warnings(h).length }
}

const isWarned = (warnSet: Set<string>, c: HealthCheck) => warnSet.has(checkId(c))

export type SubsystemGroup = {
  id: SubsystemId
  measured: HealthCheck[]
  noData: HealthCheck[]
  warnings: HealthCheck[]
}

/** Hub-level and background-job checks partitioned into the 4 subsystem cards, separating measured from insufficient. */
export function subsystemGroups(h: HubHealth): SubsystemGroup[] {
  const warnSet = new Set(warnings(h).map(checkId))
  const checks = h.checks.filter(known)
  return SUBSYSTEM_IDS.map((id) => {
    const allowed = new Set<HealthKey>(SUBSYSTEM_KEYS[id])
    const items = checks.filter((c) => {
      if (!allowed.has(c.key)) return false
      if (c.subject == null) return true
      return id === "clientAndJobs" && c.key.startsWith("job_")
    })
    const measured = items
      .filter((c) => c.value != null)
      .map((c, index) => ({ c, index, warned: isWarned(warnSet, c) }))
      .sort((a, b) => Number(b.warned) - Number(a.warned) || a.index - b.index)
      .map((x) => x.c)
    const noData = items.filter((c) => c.value == null)
    const groupWarnings = measured.filter((c) => isWarned(warnSet, c))
    return { id, measured, noData, warnings: groupWarnings }
  })
}

export type NodeHealthRow = {
  node: string
  link: HealthCheck | null
  linkDrops: HealthCheck | null
  commandFailures: HealthCheck | null
  backlog: HealthCheck | null
  diskWrites: HealthCheck | null
  warnings: HealthCheck[]
}

export function nodeHealthMatrix(h: HubHealth): NodeHealthRow[] {
  const warnSet = new Set(warnings(h).map(checkId))
  const byNode = new Map<string, NodeHealthRow>()
  for (const c of h.checks.filter(known)) {
    if (!c.key.startsWith("node_") || !c.subject) continue
    let row = byNode.get(c.subject)
    if (!row) {
      row = { node: c.subject, link: null, linkDrops: null, commandFailures: null, backlog: null, diskWrites: null, warnings: [] }
      byNode.set(c.subject, row)
    }
    if (c.key === "node_link") row.link = c
    else if (c.key === "node_link_drops") row.linkDrops = c
    else if (c.key === "node_command_failures") row.commandFailures = c
    else if (c.key === "node_backlog") row.backlog = c
    else if (c.key === "node_disk_writes") row.diskWrites = c
    if (isWarned(warnSet, c)) row.warnings.push(c)
  }
  return [...byNode.values()].sort((a, b) => {
    const warnDiff = b.warnings.length - a.warnings.length
    if (warnDiff !== 0) return warnDiff
    const aDown = a.link?.value === 0 ? 1 : 0
    const bDown = b.link?.value === 0 ? 1 : 0
    if (bDown !== aDown) return bDown - aDown
    const aLocal = a.node === "local" ? 0 : 1
    const bLocal = b.node === "local" ? 0 : 1
    if (aLocal !== bLocal) return aLocal - bLocal
    return a.node.localeCompare(b.node)
  })
}

export type PluginHealthRow = {
  plugin: string
  errors: HealthCheck | null
  restarts: HealthCheck | null
  backlog: HealthCheck | null
  warnings: HealthCheck[]
}

export function pluginHealthMatrix(h: HubHealth): PluginHealthRow[] {
  const warnSet = new Set(warnings(h).map(checkId))
  const byPlugin = new Map<string, PluginHealthRow>()
  for (const c of h.checks.filter(known)) {
    if (!c.key.startsWith("plugin_") || !c.subject) continue
    let row = byPlugin.get(c.subject)
    if (!row) {
      row = { plugin: c.subject, errors: null, restarts: null, backlog: null, warnings: [] }
      byPlugin.set(c.subject, row)
    }
    if (c.key === "plugin_errors") row.errors = c
    else if (c.key === "plugin_restarts") row.restarts = c
    else if (c.key === "plugin_backlog") row.backlog = c
    if (isWarned(warnSet, c)) row.warnings.push(c)
  }
  return [...byPlugin.values()].sort((a, b) => b.warnings.length - a.warnings.length || a.plugin.localeCompare(b.plugin))
}

export type GoldenSeries = {
  key: GoldenKey
  check: HealthCheck | null
  warned: boolean
  values: (number | null)[]
}

/** Extracts one golden metric's chart series in display units (ms for latency, % for CPU, bytes for memory). */
export function goldenSeries(h: HubHealth, key: GoldenKey): GoldenSeries {
  const check = h.checks.find((c) => c.key === key && c.subject == null) ?? null
  const warned = check ? warnings(h).some((w) => w.key === check.key && w.subject === check.subject) : false
  const points = h.series ?? []
  const values = points.map((p) => {
    const raw = p[key]
    if (raw == null || !Number.isFinite(raw)) return null
    if (key === "http_p95" || key === "write_lock_wait_p95") return raw * 1000
    if (key === "hub_cpu") return raw * 100
    return raw
  })
  return { key, check, warned, values }
}
