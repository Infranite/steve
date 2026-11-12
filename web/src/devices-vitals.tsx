/* 设备状态（B3）：负载、内存、各根目录剩余空间、往返时间，以及最近一天的走势。
 * 一张面板（panel.tsx）：每项一行，名称和说明在左，一天的走势线居中，右边等宽数字带一条用量细条；
 * 用量过高时细条变橙（≥ 75%）或变红（≥ 90%）。底下的数字条是会话数、CPU 数和内存总量。 */
import { useEffect, useState, type ReactNode } from "react"
import { ActivityIcon } from "lucide-react"
import { api, type RVitals } from "@/api"
import { MeterValue, Panel, PanelRow, PanelSection, Spark, StatStrip, type Tone } from "@/panel"
import { useT } from "@/i18n"
import { tm } from "@/store"

const gb = (b: number) => (b / 2 ** 30).toFixed(b >= 100 * 2 ** 30 ? 0 : 1)
const pct = (x: number) => Math.round(x * 100) + "%"
/** how full: orange from 75%, red from 90% */
export const fill = (x: number | null | undefined): Tone | undefined => (x == null ? undefined : x >= 0.9 ? "bad" : x >= 0.75 ? "wait" : undefined)

/** the vitals of `node` ("local" for the Hub's own machine, else a remote Node's id), reloaded every minute;
 *  `undefined` while loading, `null` when the Node has sent none */
export function useVitals(node: string) {
  const [v, setV] = useState<RVitals | null | undefined>(undefined)
  useEffect(() => {
    let live = true
    const load = () => api.vitals().then(r => { if (live) setV(r.nodes[node] ?? null) }).catch(() => { if (live) setV(null) })
    void load()
    const timer = setInterval(load, 60_000)
    return () => { live = false; clearInterval(timer) }
  }, [node])
  return v
}

/** the figures a vitals report gives: load per CPU, memory used, the disks (roots on one file system read the same: one row) */
export function vitalsFacts(v: RVitals) {
  const l = v.latest
  const memUsed = l.mem_total && l.mem_available != null ? 1 - l.mem_available / l.mem_total : null
  const loadRatio = l.load && l.cpus ? l.load[0] / l.cpus : null
  const disks: { names: string[]; free: number; total: number }[] = []
  for (const r of l.roots ?? []) {
    const same = disks.find(d => d.free === r.free && d.total === r.total)
    const name = r.display || r.root
    if (!same) disks.push({ names: [name], free: r.free, total: r.total })
    else if (!same.names.includes(name)) same.names.push(name)
  }
  const fullest = disks.reduce<number | null>((m, d) => (d.total ? Math.max(m ?? 0, 1 - d.free / d.total) : m), null)
  return { l, memUsed, loadRatio, disks, fullest }
}

/** 设备状态 as a panel; `right` goes in its head (a state tag), `children` after its figures (a foot with actions) */
export function VitalsSection({ node, right, children }: { node: string; right?: ReactNode; children?: ReactNode }) {
  const t = useT()
  const v = useVitals(node)
  if (v === undefined) return null
  if (v === null) return (
    <Panel k="vitals" data-vitals icon={<ActivityIcon />} title={t("shell:devices.vitals.title")} sub={t("shell:devices.vitals.none")} right={right}>{children}</Panel>
  )
  const { l, memUsed, loadRatio, disks } = vitalsFacts(v)
  const series = v.series
  return (
    <Panel k="vitals" data-vitals icon={<ActivityIcon />} title={t("shell:devices.vitals.title")} sub={t("shell:devices.vitals.at", { time: tm(l.at) })} right={right}>
      <PanelSection flush label={t("shell:devices.vitals.metric")} meta={t("shell:devices.vitals.window")}>
        {l.load && <PanelRow title={t("shell:devices.vitals.load")} sub={t("shell:devices.vitals.loadSub", { load: l.load[0].toFixed(2), cpus: l.cpus ?? "?" })} data-vital="load">
          <Spark values={series.map(p => p.load)} label={t("shell:devices.vitals.load")} className="max-sm:hidden" /><MeterValue value={l.load[0].toFixed(2)} ratio={loadRatio} tone={fill(loadRatio)} /></PanelRow>}
        {memUsed != null && <PanelRow title={t("shell:devices.vitals.memory")} sub={t("shell:devices.vitals.memorySub", { used: pct(memUsed), free: gb(l.mem_available ?? 0), total: gb(l.mem_total ?? 0) })} data-vital="memory">
          <Spark values={series.map(p => p.mem)} label={t("shell:devices.vitals.memory")} className="max-sm:hidden" /><MeterValue value={pct(memUsed)} ratio={memUsed} tone={fill(memUsed)} /></PanelRow>}
        {disks.map(d => { const used = d.total ? 1 - d.free / d.total : 0; return (
          <PanelRow key={d.names.join()} title={t("shell:devices.vitals.disk", { root: d.names.join(t("shell:list.sep")) })} sub={t("shell:devices.vitals.diskSub", { free: gb(d.free), total: gb(d.total) })} data-vital="disk">
            <MeterValue value={pct(used)} ratio={used} tone={fill(used)} /></PanelRow>) })}
        {l.rtt_ms != null && <PanelRow title={t("shell:devices.vitals.rtt")} sub={t("shell:devices.vitals.rttSub")} data-vital="rtt">
          <Spark values={series.map(p => p.rtt == null ? null : Math.min(1, p.rtt / 1000))} label={t("shell:devices.vitals.rtt")} className="max-sm:hidden" /><MeterValue value={l.rtt_ms + " ms"} ratio={Math.min(1, l.rtt_ms / 1000)} tone={l.rtt_ms >= 500 ? "wait" : undefined} /></PanelRow>}
      </PanelSection>
      <StatStrip items={[
        { k: "sessions", label: t("shell:devices.vitals.sessions"), value: String(l.sessions ?? 0) },
        { k: "cpus", label: t("shell:devices.vitals.cpus"), value: l.cpus ?? "—" },
        { k: "mem", label: t("shell:devices.vitals.memTotal"), value: l.mem_total ? gb(l.mem_total) + " GB" : "—" },
      ]} />
      {children}
    </Panel>
  )
}

/** the vitals in one strip (the Hub's card on the device list): load, memory, the fullest disk, sessions */
export function VitalsStrip({ node }: { node: string }) {
  const t = useT()
  const v = useVitals(node)
  if (!v) return null
  const { l, memUsed, loadRatio, fullest } = vitalsFacts(v)
  return (
    <StatStrip items={[
      { k: "load", label: t("shell:devices.vitals.load"), value: l.load ? l.load[0].toFixed(2) : "—", tone: fill(loadRatio), sub: l.cpus ? t("shell:devices.vitals.cpusShort", { n: l.cpus }) : undefined },
      { k: "memory", label: t("shell:devices.vitals.memory"), value: memUsed != null ? pct(memUsed) : "—", tone: fill(memUsed) },
      { k: "disk", label: t("shell:devices.vitals.diskShort"), value: fullest != null ? pct(fullest) : "—", tone: fill(fullest) },
      { k: "sessions", label: t("shell:devices.vitals.sessions"), value: String(l.sessions ?? 0) },
    ]} />
  )
}
