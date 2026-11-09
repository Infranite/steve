/* 设备状态（B3）：负载、内存、各根目录剩余空间、往返时间，以及最近一天的走势。 */
import { useEffect, useState } from "react"
import { api, type RVitals } from "@/api"
import { Group, Row, SectionLabel } from "@/ui"
import { useT } from "@/i18n"
import { tm } from "@/store"

const gb = (b: number) => (b / 2 ** 30).toFixed(b >= 100 * 2 ** 30 ? 0 : 1)
const pct = (x: number) => Math.round(x * 100) + "%"

/* 一条小折线：0..1 的序列，空值断开 */
function Spark({ values, label }: { values: (number | null)[]; label: string }) {
  if (values.filter(v => v != null).length < 2) return null
  const pts = values.map((v, i) => v == null ? null : [i / Math.max(1, values.length - 1) * 100, 24 - Math.min(1, Math.max(0, v)) * 22] as const)
  const d = pts.reduce((s, p, i) => p ? s + (i && pts[i - 1] ? "L" : "M") + p[0].toFixed(1) + " " + p[1].toFixed(1) : s, "")
  return <svg viewBox="0 0 100 26" preserveAspectRatio="none" className="h-6 w-24 shrink-0 text-muted-foreground" role="img" aria-label={label}><path d={d} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" /></svg>
}

/** `node`: "local" for the Hub's own machine, else a remote Node's id. */
export function VitalsSection({ node }: { node: string }) {
  const t = useT()
  const [v, setV] = useState<RVitals | null | undefined>(undefined)
  useEffect(() => {
    let live = true
    const load = () => api.vitals().then(r => { if (live) setV(r.nodes[node] ?? null) }).catch(() => { if (live) setV(null) })
    void load()
    const timer = setInterval(load, 60_000)
    return () => { live = false; clearInterval(timer) }
  }, [node])
  if (v === undefined) return null
  if (v === null) return <section data-vitals><SectionLabel>{t("shell:devices.vitals.title")}</SectionLabel><Group><Row title={t("shell:devices.vitals.none")} /></Group></section>
  const l = v.latest, series = v.series
  const memUsed = l.mem_total && l.mem_available != null ? 1 - l.mem_available / l.mem_total : null
  // roots on one file system read the same: one row for them
  const disks: { names: string[]; free: number; total: number }[] = []
  for (const r of l.roots ?? []) {
    const same = disks.find(d => d.free === r.free && d.total === r.total)
    const name = r.display || r.root
    if (!same) disks.push({ names: [name], free: r.free, total: r.total })
    else if (!same.names.includes(name)) same.names.push(name)
  }
  return (
    <section data-vitals>
      <SectionLabel right={<span className="type-footnote font-normal">{t("shell:devices.vitals.at", { time: tm(l.at) })}</span>}>{t("shell:devices.vitals.title")}</SectionLabel>
      <Group>
        {l.load && <Row title={t("shell:devices.vitals.load")} sub={t("shell:devices.vitals.loadSub", { load: l.load[0].toFixed(2), cpus: l.cpus ?? "?" })}><Spark values={series.map(p => p.load)} label={t("shell:devices.vitals.load")} /></Row>}
        {memUsed != null && <Row title={t("shell:devices.vitals.memory")} sub={t("shell:devices.vitals.memorySub", { used: pct(memUsed), free: gb(l.mem_available ?? 0), total: gb(l.mem_total ?? 0) })}><Spark values={series.map(p => p.mem)} label={t("shell:devices.vitals.memory")} /></Row>}
        {disks.map(d => <Row key={d.names.join()} title={t("shell:devices.vitals.disk", { root: d.names.join(t("shell:list.sep")) })} sub={t("shell:devices.vitals.diskSub", { free: gb(d.free), total: gb(d.total) })} />)}
        <Row title={t("shell:devices.vitals.sessions")} sub={String(l.sessions ?? 0)} />
        {l.rtt_ms != null && <Row title={t("shell:devices.vitals.rtt")} sub={l.rtt_ms + " ms"}><Spark values={series.map(p => p.rtt == null ? null : Math.min(1, p.rtt / 1000))} label={t("shell:devices.vitals.rtt")} /></Row>}
      </Group>
    </section>
  )
}
