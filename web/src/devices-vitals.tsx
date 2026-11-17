/* How a Node's machine is doing, from the Hub's samples (about one a minute, a day kept): the figures now (sessions, CPU
 * load, memory, the fullest project disk, the round trip to the Hub) and the day as three charts (affectUI KPI Chart).
 * Offline, the figures are its last data under a note that says so; a stale sample says when it stopped. */
import { useT } from "@/i18n"
import { date, num } from "@/i18n/format"
import { KpiChart } from "@/components/affect/kpi-chart-clean"
import { Tooltip } from "@/components/affect/tooltip-clean"
import { ActivityIcon } from "lucide-react"
import { agoText, disks, linkRoute, loadPct, memPct, one, rttOf, type NodeView } from "@/device-model"
import { useNodeVitals } from "@/devices-data"
import { Alert, DCard, Figure } from "@/devices-ui"

const GIB = 2 ** 30

/** the figures now: sessions against the limit, CPU load, memory, the fullest disk, the round trip */
export function NodeFigures({ v }: { v: NodeView }) {
  const t = useT(), { value: vit, stale } = useNodeVitals(v.id)
  const l = vit?.latest, lp = loadPct(l), mp = memPct(l), disk = disks(l)[0], rtt = rttOf(v, l), route = linkRoute(v)
  const offline = !v.linked && !v.local
  const at = l ? l.sampled_at ?? l.at : null
  return (
    <DCard data-node-figures className="p-0">
      <div className="flex flex-col gap-4 p-5">
        {offline && <Alert tone="warn" data-node-offline>
          <strong className="font-semibold">{v.lastSeen ? t("devices:status.offline", { ago: agoText(v.lastSeen) }) : t("devices:status.offlineNever")}</strong>
          {v.linkError && <span className="block font-mono type-footnote text-muted-foreground [overflow-wrap:anywhere]" data-node-link-error>{t("devices:node.lastFailure", { ago: agoText(v.linkError.at) })} {v.linkError.error}</span>}
          <span className="block type-footnote text-muted-foreground">{t("devices:node.offlineNote")}</span>
        </Alert>}
        <div className="grid grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-4">
          {/* the round trip sits under the sessions: both say how the Hub reaches it and what it gives it */}
          <div className="row-span-2 flex min-w-0 flex-col gap-4">
            <Figure label={t("devices:stat.sessions")} value={v.running ?? "—"} unit={v.max != null ? ` / ${v.max}` : undefined} fill={v.max && v.running != null ? v.running / v.max : null} data-running={v.running} data-max={v.max} data-waiting={v.waiting}
              sub={(v.waiting ?? 0) > 0 ? <span className="text-attention">{t("devices:stat.queued", { n: v.waiting })}</span> : t("devices:stat.noQueue")} />
            {!v.local && <Figure label={t("devices:vitals.rtt")} value={rtt ?? "—"} unit={rtt != null ? " ms" : undefined} sub={route ? t(`devices:route.${route}`) : undefined} data-vital="rtt" />}
          </div>
          <Tooltip content={l?.load ? t("devices:vitals.cpuTip", { values: l.load.map((x) => num(one(x))).join(" / ") }) : t("devices:vitals.none")}>
            <div tabIndex={0} className="min-w-0 outline-none"><Figure label={t("devices:vitals.cpu")} value={lp != null ? `${lp}%` : "—"} fill={lp != null ? lp / 100 : null} data-vital="cpu"
              sub={l?.load && l.cpus ? t("devices:vitals.cpuSub", { load: num(one(l.load[0])), cpus: l.cpus }) : offline ? t("devices:vitals.noData") : undefined} /></div>
          </Tooltip>
          <Figure label={t("devices:vitals.mem")} value={mp != null ? `${mp}%` : "—"} fill={mp != null ? mp / 100 : null} data-vital="memory"
            sub={l?.mem_total && l.mem_available != null ? t("devices:vitals.memSub", { used: Math.round((l.mem_total - l.mem_available) / GIB), total: Math.round(l.mem_total / GIB) }) : undefined} />
          {disk ? <Figure label={<>{t("devices:vitals.disk")} <span className="type-code text-tertiary">{disk.name}</span></>} value={t("devices:vitals.diskFree", { n: num(disk.free) })} fill={disk.used} data-vital="disk"
            sub={t("devices:vitals.diskTotal", { n: num(disk.total) })} />
            : <Figure label={t("devices:vitals.disk")} value="—" data-vital="disk" />}
        </div>
        {stale && at && !offline && <p className="type-footnote text-attention" title={date(at, { dateStyle: "medium", timeStyle: "medium" })} data-vitals-stale>{t("devices:vitals.stale", { ago: agoText(at) })}</p>}
        {vit === null && <p className="type-footnote text-muted-foreground" data-sample-state="none">{t("devices:vitals.none")}</p>}
      </div>
    </DCard>
  )
}

const hhmm = (at: number) => date(at, { hour: "2-digit", minute: "2-digit" })

/** the day: CPU load, memory and (for a remote Node) the round trip; a missing sample is a gap, never a zero */
export function NodeTrends({ v }: { v: NodeView }) {
  const t = useT(), { value: vit, stale } = useNodeVitals(v.id)
  if (!vit || !vit.series.length) return null
  const s = vit.series, labels = s.map((p) => hhmm(p.at)), tips = s.map((p) => date(p.at, { dateStyle: "medium", timeStyle: "short" }))
  const last = <K extends "load" | "mem" | "rtt">(k: K) => [...s].reverse().find((p) => p[k] != null)?.[k] ?? null
  const pct = (n: number) => `${Math.round(n)}%`
  const cpu = last("load"), mem = last("mem"), rtt = last("rtt")
  const every = vit.sampling ? Math.round(vit.sampling.interval_ms / 1000) : null
  const sub = [every ? t("devices:trends.every", { s: every }) : "", stale ? t("devices:vitals.stale", { ago: agoText(vit.latest.at) }) : t("devices:trends.fresh")].filter(Boolean).join(" · ")
  const common = { labels, tooltipLabels: tips, emptyText: t("devices:vitals.none"), missingText: "—" }
  return (
    <DCard icon={<ActivityIcon />} title={t("devices:trends.title")} sub={sub} data-node-trends>
      <div className={`grid gap-3 px-5 pb-5 ${v.local ? "md:grid-cols-2" : "md:grid-cols-3"}`}>
        <KpiChart {...common} title={t("devices:vitals.cpu")} value={cpu != null ? pct(cpu * 100) : "—"} format={pct}
          series={[{ label: t("devices:vitals.cpu"), color: "var(--series-1)", values: s.map((p) => (p.load == null ? null : p.load * 100)) }]} />
        <KpiChart {...common} title={t("devices:vitals.mem")} value={mem != null ? pct(mem * 100) : "—"} format={pct}
          series={[{ label: t("devices:vitals.mem"), color: "var(--series-2)", values: s.map((p) => (p.mem == null ? null : p.mem * 100)) }]} />
        {!v.local && <KpiChart {...common} title={t("devices:vitals.rtt")} value={rtt != null ? `${Math.round(rtt)} ms` : "—"} format={(n) => `${Math.round(n)} ms`}
          series={[{ label: t("devices:vitals.rtt"), color: "var(--series-3)", values: s.map((p) => p.rtt) }]} />}
      </div>
    </DCard>
  )
}
