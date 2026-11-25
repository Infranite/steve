/* Settings · 关于 · 运行状况: the Hub's `/api/system/health` (a few figures over its last window, each with its
 * limit, and the ones past it) and how the page words them. */
/* relative .ts imports so node --test loads this file without a bundler */
import { t } from "./i18n/index.ts"
import { msDur } from "./i18n/format.ts"
import { formatBytes } from "./devices-install.ts"

export const KEYS = [
  "http_p95", "write_lock_wait_p95", "sse_delivery_p95", "patch_delay_p95", "reconcile_diff", "ingest_p95", "ingest_rate", "cache_hit_ratio",
  "hub_cpu", "hub_memory", "hub_disk_writes", "web_interaction_p75", "web_long_tasks",
  "job_panics", "jobs_down", "job_up",
  "node_link", "node_link_drops", "node_command_failures", "node_backlog", "node_disk_writes",
  "plugin_errors", "plugin_restarts", "plugin_backlog",
] as const
export type HealthKey = (typeof KEYS)[number]
export type HealthCheck = { key: HealthKey; subject?: string; value: number | null; unit: string; max?: number; min?: number }
export type HubHealth = { window_seconds: number; checks: HealthCheck[]; warnings: HealthCheck[] }

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

/** "上限 500 ms" / "下限 50%", or nothing for a figure shown only for information */
export function limitText(c: HealthCheck): string | null {
  if (c.max != null) return t("shell:health.max", { limit: shown(c.unit, c.max) })
  if (c.min != null) return t("shell:health.min", { limit: shown(c.unit, c.min) })
  return null
}

/** The figure, or that the window had too few samples to say */
export const valueText = (c: HealthCheck) => (c.value == null ? t("shell:health.noData") : shown(c.unit, c.value))

/** Why a figure past its limit matters, in a sentence */
export const warningText = (c: HealthCheck) =>
  t(`shell:health.warn.${c.key}`, { subject: c.subject ?? "", value: valueText(c), limit: shown(c.unit, limitOf(c) ?? 0) })

export const label = (c: HealthCheck) => t(`shell:health.key.${c.key}`)

/** The Hub's own figures, then each Node's and each plugin's, in the order the Hub gave them; keys this page does
 * not know (a newer Hub) are left out. */
export function grouped(h: HubHealth): { title: string; checks: HealthCheck[] }[] {
  const out: { title: string; checks: HealthCheck[] }[] = []
  for (const c of h.checks.filter(known)) {
    const title = c.subject == null ? t("shell:health.hub")
      : c.key.startsWith("node_") ? t("shell:health.node", { name: c.subject })
      : c.key.startsWith("job_") ? t("shell:health.job", { name: c.subject })
      : t("shell:health.plugin", { name: c.subject })
    const g = out.find((x) => x.title === title)
    if (g) g.checks.push(c)
    else out.push({ title, checks: [c] })
  }
  return out
}

export const warnings = (h: HubHealth) => h.warnings.filter(known)
