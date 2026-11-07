/** Settings · 数据 · 工作区与磁盘 (ST-45): the Hub's `/api/system/workspaces` report and what the page says about it. */
/* a relative .ts import so node --test loads this file without a bundler */
import { t } from "./i18n/index.ts"

export type DiskPolicy = { floor: number; low: number; idle_ms: number }
export type Pressure = "ok" | "low" | "floor"
export type DiskEntry = {
  item: string; workspace: string; title: string; status: string; phase: string; archived: boolean; branch: string | null
  path: string; exists: boolean; bytes: number; cache_bytes: number; caches: string[]; busy: boolean; in_use: boolean | null; updated: number
}
export type DiskReport = {
  root: string; free: number | null; total: number | null; pressure: Pressure | null; policy: DiskPolicy; git: string | null
  trash: number; entries: DiskEntry[]; orphans: { path: string; bytes: number }[]; measured_at: number
  /** items without a project, in their scratch directory (absent from a Hub before this) */
  scratch?: DiskEntry[]
}
export type Freed = { item: string; caches: string[]; bytes: number }
export type Reclaimed = { skipped: boolean; free_before: number | null; free_after: number | null; freed: Freed[]; removed: string[]; kept: { item: string; reason: string }[] }

const GB = 2 ** 30

/** "12 GB" / "3.4 GB" / "512 MB", binary units like the Hub's notes */
export function bytes(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return "—"
  const g = n / GB
  if (g >= 10) return `${Math.round(g)} GB`
  if (g >= 0.1) return `${g.toFixed(1)} GB`
  return `${Math.round(n / 2 ** 20)} MB`
}

/** What a worktree is doing, in the words of the list; the order is the list's order. */
export type EntryState = "busy" | "in_use" | "open" | "closed" | "gone"
export const stateLabel = (s: EntryState): string => t(`shell:disk.state.${s}`)
export function entryState(e: DiskEntry): EntryState {
  if (!e.exists) return "gone"
  if (e.busy) return "busy"
  if (e.in_use) return "in_use"
  return e.phase === "closed" ? "closed" : "open"
}

/** The list: worktrees that are still on disk, largest first; the ones already given back only count. */
export function diskRows(r: DiskReport) {
  const on = r.entries.filter((e) => e.exists).sort((a, b) => b.bytes - a.bytes)
  const total = on.reduce((s, e) => s + e.bytes, 0), cache = on.reduce((s, e) => s + e.cache_bytes, 0)
  /* a closed item's caches go on the next pass unless something still runs in it */
  const reclaimable = on.filter((e) => entryState(e) === "closed").reduce((s, e) => s + e.cache_bytes, 0)
  return { rows: on, total, cache, reclaimable, gone: r.entries.length - on.length }
}

/** Scratch directories (items without a project) on disk, largest first; an ended item's marked caches go
 * only while the disk is under pressure. */
export function scratchRows(r: DiskReport) {
  const on = (r.scratch ?? []).filter((e) => e.exists).sort((a, b) => b.bytes - a.bytes)
  const total = on.reduce((s, e) => s + e.bytes, 0), cache = on.reduce((s, e) => s + e.cache_bytes, 0)
  const ended = on.filter((e) => entryState(e) === "closed").reduce((s, e) => s + e.cache_bytes, 0)
  return { rows: on, total, cache, ended }
}

/** One line on the disk against the thresholds. */
export function pressureText(r: DiskReport): string {
  if (r.free === null) return t("shell:disk.noFree")
  const p = r.policy
  if (r.pressure === "floor") return t("shell:disk.floor", { free: bytes(r.free), floor: bytes(p.floor) })
  if (r.pressure === "low") return t("shell:disk.low", { free: bytes(r.free), low: bytes(p.low), count: Math.round(p.idle_ms / 3_600_000) })
  return t("shell:disk.ok", { free: bytes(r.free), low: bytes(p.low), floor: bytes(p.floor) })
}

/** The toast after "立即回收". */
export function reclaimText(r: Reclaimed): string {
  if (r.skipped) return t("shell:disk.skipped")
  const freed = r.freed.reduce((s, f) => s + f.bytes, 0)
  const parts = [freed > 0 ? t("shell:disk.freed", { bytes: bytes(freed) }) : t("shell:disk.nothingFreed")]
  if (r.removed.length) parts.push(t("shell:disk.removed", { count: r.removed.length }))
  if (r.kept.length) parts.push(t("shell:disk.kept", { count: r.kept.length }))
  return parts.join(t("shell:disk.sep"))
}
