import type { RVitals } from "./api.ts"
export const gib = (b: number) => b / 2 ** 30
export function vitalsFacts(v: RVitals) {
  const l = v.latest
  const memUsed = l.mem_total && l.mem_available != null && l.mem_available >= 0 && l.mem_available <= l.mem_total ? 1 - l.mem_available / l.mem_total : null
  const loadRatio = l.load && l.cpus && l.cpus > 0 ? l.load[0] / l.cpus : null
  // Equal capacities do not prove two roots share a filesystem.
  const disks = (l.roots ?? []).filter(r => r.total > 0 && r.free >= 0 && r.free <= r.total).map(r => ({ ...r, names: [r.display || r.root] }))
  const fullest = disks.reduce<number | null>((m, d) => Math.max(m ?? 0, 1 - d.free / d.total), null)
  return { l, memUsed, loadRatio, disks, fullest }
}
export function sampleState(v: RVitals | null | undefined, failed: boolean, now: number) {
  if (failed) return "failed"
  if (v === undefined) return "loading"
  if (!v) return "none"
  if (!v.sampling) return "unknown"
  return now - v.latest.at > v.sampling.stale_after_ms ? "stale" : "fresh"
}
/** Geometry only; every label stays HTML so it grows naturally with text size. */
export function chartPaths(v: RVitals, metric: "load" | "mem" | "rtt") {
  const series = v.series, start = series[0]?.at ?? 0, end = series.at(-1)?.at ?? start
  const max = Math.max(metric === "mem" ? 1 : 0.01, ...series.map(p => p[metric] ?? 0))
  const paths: string[] = []
  let path = "", previous: number | null = null
  for (const p of series) {
    const value = p[metric]
    const gap = previous != null && (v.gaps?.some(g => g.from < p.at && g.to > previous!) ?? false)
    if (value == null || !Number.isFinite(value)) { if (path) paths.push(path); path = ""; previous = null; continue }
    if (gap && path) { paths.push(path); path = "" }
    const x = end === start ? 50 : (p.at - start) / (end - start) * 100, y = 100 - Math.max(0, value) / max * 100
    path += `${path ? " L" : "M"}${x.toFixed(2)},${y.toFixed(2)}`
    previous = p.at
  }
  if (path) paths.push(path)
  return { paths, max, start, end }
}
