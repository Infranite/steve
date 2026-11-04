/** Settings · 数据 · 工作区与磁盘 (ST-45): the Hub's `/api/system/workspaces` report and what the page says about it. */

export type DiskPolicy = { floor: number; low: number; idle_ms: number }
export type Pressure = "ok" | "low" | "floor"
export type DiskEntry = {
  item: string; workspace: string; title: string; status: string; phase: string; archived: boolean; branch: string | null
  path: string; exists: boolean; bytes: number; cache_bytes: number; caches: string[]; busy: boolean; in_use: boolean | null; updated: number
}
export type DiskReport = {
  root: string; free: number | null; total: number | null; pressure: Pressure | null; policy: DiskPolicy; git: string | null
  trash: number; entries: DiskEntry[]; orphans: { path: string; bytes: number }[]; measured_at: number
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
export const STATE_LABEL: Record<EntryState, string> = { busy: "运行中", in_use: "有进程在用", open: "进行中", closed: "已结束", gone: "已回收" }
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

/** One line on the disk against the thresholds. */
export function pressureText(r: DiskReport): string {
  if (r.free === null) return "读不到这块磁盘的剩余空间"
  const p = r.policy
  if (r.pressure === "floor") return `只剩 ${bytes(r.free)}，低于下限 ${bytes(p.floor)}：暂停新建工作区，回收闲置缓存`
  if (r.pressure === "low") return `剩余 ${bytes(r.free)}，低于 ${bytes(p.low)}：回收闲置 ${Math.round(p.idle_ms / 3_600_000)} 小时以上事项的缓存`
  return `剩余 ${bytes(r.free)}，充足；低于 ${bytes(p.low)} 回收闲置缓存，低于 ${bytes(p.floor)} 暂停新建工作区`
}

/** The toast after "立即回收". */
export function reclaimText(r: Reclaimed): string {
  if (r.skipped) return "已有一次回收在进行，稍后再看"
  const freed = r.freed.reduce((s, f) => s + f.bytes, 0)
  const parts = [freed > 0 ? `释放了 ${bytes(freed)}` : "没有可回收的缓存"]
  if (r.removed.length) parts.push(`删除了 ${r.removed.length} 个工作区（分支都保留）`)
  if (r.kept.length) parts.push(`${r.kept.length} 个按规则保留`)
  return parts.join("，")
}
