import { useEffect, useState, type ReactNode } from "react"
import { api, type RVitals } from "@/api"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { useT } from "@/i18n"
import { date, num } from "@/i18n/format"
import { chartPaths, gib, sampleState, vitalsFacts } from "@/device-vitals-model"
import "./devices.css"
export { vitalsFacts } from "@/device-vitals-model"
export const fill = (x: number | null | undefined) => x == null ? undefined : x >= .9 ? "bad" : x >= .75 ? "wait" : undefined
const at = (n: number) => date(n, { dateStyle: "short", timeStyle: "medium" })
const gb = (n: number) => num(gib(n), { maximumFractionDigits: 1 })
const pc = (n: number) => num(n * 100, { maximumFractionDigits: 1 })
export function useVitals(node: string) {
  const [snapshot, setSnapshot] = useState<{ node: string; value: RVitals | null | undefined; error: string }>({ node, value: undefined, error: "" })
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    let live = true, timer: ReturnType<typeof setTimeout> | undefined
    setSnapshot({ node, value: undefined, error: "" })
    const load = async () => {
      try {
        const r = await api.vitals()
        if (live) setSnapshot({ node, value: r.nodes[node] ?? null, error: "" })
      } catch (e) { if (live) setSnapshot(s => ({ ...s, node, error: (e as Error).message })) }
      finally { if (live) { setNow(Date.now()); timer = setTimeout(() => void load(), 60_000) } }
    }
    void load()
    const tick = setInterval(() => setNow(Date.now()), 10_000)
    return () => { live = false; clearTimeout(timer); clearInterval(tick) }
  }, [node])
  const value = snapshot.node === node ? snapshot.value : undefined, error = snapshot.node === node ? snapshot.error : ""
  return { value, error, stale: sampleState(value, false, now) === "stale", state: sampleState(value, !!error, now) }
}
function Trend({ value, metric, title }: { value: RVitals; metric: "load" | "mem" | "rtt"; title: string }) {
  const t = useT(), chart = chartPaths(value, metric)
  const label = (n: number) => num(metric === "mem" ? n * 100 : n, { maximumFractionDigits: 2 }) + (metric === "mem" ? "%" : "")
  return <figure className="device-chart" data-chart={metric}>
    <figcaption>{title}</figcaption>
    <div className="device-chart-plot"><div className="device-chart-axis" data-axis><span>{label(chart.max)}</span><span>{label(chart.max / 2)}</span><span>{label(0)}</span></div>
      <svg viewBox="0 0 100 100" preserveAspectRatio="none" role="img" aria-label={title}>
        {[0, 50, 100].map(y => <path key={y} d={`M0,${y} H100`} stroke="var(--border)" vectorEffect="non-scaling-stroke" />)}
        {chart.paths.map((p, i) => <path key={i} d={p} stroke="var(--primary)" strokeWidth="2" fill="none" vectorEffect="non-scaling-stroke" />)}
      </svg></div>
    {chart.start > 0 && <div className="device-chart-times"><time>{at(chart.start)}</time><time>{at(chart.end)}</time></div>}
    {value.gaps?.map(g => <p key={g.from} data-sampling-gap>{t("device:gaps", { from: at(g.from), to: at(g.to) })}</p>)}
    {(!value.series.length || value.series.some(p => p[metric] == null)) && <p>{t("device:missing")}</p>}
  </figure>
}
export function VitalsSection({ node, identity, right, children }: { node: string; identity?: string; right?: ReactNode; children?: ReactNode }) {
  const t = useT(), { value: v, error, stale, state } = useVitals(node)
  const [disk, setDisk] = useState<{ root: string; display?: string; free: number; total: number; at: number } | null>(null)
  const facts = v ? vitalsFacts(v) : null
  return <section className="device-vitals" data-vitals data-sample-state={state}>
    <header><h2>{t("device:monitor")}</h2>{right}<p>{t("device:nodeIdentity", { id: node })}</p></header>
    {error && <p role="alert">{t("device:readFailed", { error })}</p>}
    {state === "loading" && <p>{t("device:loading")}</p>}
    {state === "none" && <p>{t("device:noSamples")}</p>}
    {v && facts && <>
      <p data-sample-time>{t(stale ? "device:stale" : "device:fresh", { time: at(v.latest.sampled_at ?? v.latest.at) })}</p>
      <p>{t("device:sampleReceived", { time: at(v.latest.at) })}</p>
      {v.sampling ? <p>{t("device:samplePolicy", { seconds: v.sampling.interval_ms / 1000, stale: v.sampling.stale_after_ms / 1000 })}</p> : <p>{t("device:policyUnknown")}</p>}
      <div data-vital="load"><h3>{t("device:load")}</h3><p>{v.latest.load ? t("device:loadValue", { values: v.latest.load.map(n => num(n, { maximumFractionDigits: 2 })).join(" / "), cpus: v.latest.cpus ?? t("device:unknown") }) : t("device:missing")}</p></div>
      <div data-vital="memory"><h3>{t("device:memory")}</h3><p>{facts.memUsed != null ? t("device:memoryValue", { used: pc(facts.memUsed), free: gb(v.latest.mem_available!), total: gb(v.latest.mem_total!) }) : t("device:missing")}</p></div>
      <div><h3>{t("device:disk")}</h3>{facts.disks.length ? facts.disks.map(d => <div key={d.root} data-vital="disk"><p>{d.display || d.root}</p>
        <p>{t("device:diskValue", { free: gb(d.free), total: gb(d.total), used: pc(1 - d.free / d.total) })}</p>
        <meter min={0} max={d.total} low={d.total * .75} high={d.total * .9} optimum={0} value={d.total - d.free} aria-label={d.display || d.root} className="w-full" />
        <Button size="sm" variant="outline" onClick={() => setDisk({ ...d, at: v.latest.sampled_at ?? v.latest.at })} data-disk-evidence={d.root}>{t("device:diskEvidence")}</Button>
      </div>) : <p>{t("device:missing")}</p>}<p>{t("device:diskNoHistory")}</p></div>
      <details data-monitor-expanded><summary>{t("device:expandMonitor")}</summary>
        <Trend value={v} metric="load" title={t("device:perCpu")} /><Trend value={v} metric="mem" title={t("device:memory")} />
        {node !== "local" && <Trend value={v} metric="rtt" title={t("device:rtt")} />}
      </details>
    </>}
    {children}
    {disk && <Dialog open onOpenChange={open => !open && setDisk(null)}><DialogContent className="device-evidence"><DialogHeader>
      <DialogTitle>{t("device:diskEvidence")}</DialogTitle><DialogDescription>{identity} · {t("device:nodeIdentity", { id: node })}</DialogDescription></DialogHeader>
      <p>{t("device:diskRoot", { root: disk.display || disk.root })} · {disk.root}</p>
      <p>{t("device:diskValue", { free: gb(disk.free), total: gb(disk.total), used: pc(1 - disk.free / disk.total) })}</p>
      <p>{t("device:fresh", { time: at(disk.at) })}</p>
      <Button variant="outline" onClick={() => setDisk(null)}>{t("common:action.close")}</Button>
    </DialogContent></Dialog>}
  </section>
}
export function VitalsStrip({ node }: { node: string }) {
  const t = useT(), { value: v, state, stale, error } = useVitals(node)
  if (!v) return <p className="px-5 py-3">{t(error ? "device:readFailed" : state === "loading" ? "device:loading" : "device:noSamples", { error })}</p>
  const f = vitalsFacts(v)
  return <div className="px-5 py-3"><p>{t("device:load")} · {v.latest.load?.[0] ?? t("device:unknown")}</p>
    <p>{t("device:memory")} · {f.memUsed == null ? t("device:unknown") : pc(f.memUsed) + "%"}</p>
    <p>{t(stale ? "device:stale" : "device:fresh", { time: at(v.latest.sampled_at ?? v.latest.at) })}</p>{error && <p>{t("device:readFailed", { error })}</p>}
  </div>
}
