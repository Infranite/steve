/* How a Node's machine is doing, from the Hub's samples (about one a minute, a day kept):
 * - NodeFigures: the balanced snapshot on Overview (sessions, CPU load, memory, fullest disk, plus tokens & RTT)
 * - NodeResources: the deeper resource breakdown on Work & Resources (token usage, network throughput, all disk roots)
 * - NodeTrends: the last 24 hours as affectUI KPI Charts (CPU, memory, remote RTT). */
import type { ReactNode } from "react"
import { ActivityIcon, CpuIcon, HardDriveIcon } from "lucide-react"
import { useT } from "@/i18n"
import { date, num } from "@/i18n/format"
import { KpiChart } from "@/components/affect/kpi-chart-clean"
import { Tooltip } from "@/components/affect/tooltip-clean"
import { Meter } from "@/panel"
import { agoText, disks, fillTone, linkRoute, loadPct, memPct, one, rttOf, type NodeView } from "@/device-model"
import { useNodeVitals } from "@/devices-data"
import { DCard, Figure } from "@/devices-ui"
import { fmtBytes, fmtUsd } from "@/stats/format"
import { getSession } from "@/store"

const GIB = 2 ** 30

function bytesPerSecond(bytes: number) {
  const [unit, divisor] = bytes >= 1e6 ? ["MB/s", 1e6] : bytes >= 1e3 ? ["KB/s", 1e3] : ["B/s", 1]
  return `${num(bytes / divisor, { maximumFractionDigits: 1 })} ${unit}`
}

const compactTokens = (n: number) => num(n, { notation: "compact", maximumFractionDigits: 1 })

/** Instant health snapshot on Overview: sessions, CPU load, memory, fullest disk, plus compact tokens & RTT line. */
export function NodeFigures({ v, action }: { v: NodeView; action?: ReactNode }) {
  const t = useT(), { value: vit, stale } = useNodeVitals(v.id)
  const offline = !v.linked && !v.local
  const l = vit?.latest, lp = loadPct(l), mp = memPct(l), disk = disks(l)[0], rtt = offline ? null : rttOf(v, l), route = linkRoute(v)
  const tokens = vit?.tokens
  const procDisk = l?.process?.disk_written_bytes
  const adapterDisk = l?.adapter_disk_written_bytes ?? 0
  const hasDiskWrites = procDisk != null || adapterDisk > 0
  const diskWritten = (procDisk ?? 0) + adapterDisk
  const hasCost = tokens != null && (tokens.total_cost_usd > 0 || tokens.today_cost_usd > 0)
  const at = l ? l.sampled_at ?? l.at : null
  const showFooter = (!v.local && rtt != null) || !!tokens || hasDiskWrites || hasCost || (stale && !!at && !offline) || vit === null
  return (
    <DCard icon={<CpuIcon />} title={t("devices:nodeOverview.metricsTitle")} sub={offline && l ? t("devices:vitals.offlineSub") : undefined} action={action} data-node-figures>
      <div className="flex flex-col gap-4 px-5 pb-5">
        <div className="grid grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-4">
          <Figure label={t("devices:stat.sessions")} value={v.running ?? "—"} unit={v.max != null ? ` / ${v.max}` : undefined}
            fill={v.max && v.running != null ? v.running / v.max : null} data-running={v.running} data-max={v.max} data-waiting={v.waiting}
            sub={(v.waiting ?? 0) > 0 ? <span className="text-attention">{t("devices:stat.queued", { n: v.waiting })}</span> : t("devices:stat.noQueue")} />
          <Tooltip content={l?.load ? t("devices:vitals.cpuTip", { values: l.load.map((x) => num(one(x))).join(" / ") }) : t("devices:vitals.none")}>
            <div tabIndex={0} className="min-w-0 outline-none">
              <Figure label={t("devices:vitals.cpu")} value={lp != null ? `${lp}%` : "—"} fill={lp != null ? lp / 100 : null} data-vital="cpu"
                sub={l?.load && l.cpus ? t("devices:vitals.cpuSub", { load: num(one(l.load[0])), cpus: l.cpus }) : offline ? t("devices:vitals.noData") : undefined} />
            </div>
          </Tooltip>
          <Figure label={t("devices:vitals.mem")} value={mp != null ? `${mp}%` : "—"} fill={mp != null ? mp / 100 : null} data-vital="memory"
            sub={l?.mem_total && l.mem_available != null ? t("devices:vitals.memSub", { used: Math.round((l.mem_total - l.mem_available) / GIB), total: Math.round(l.mem_total / GIB) }) : undefined} />
          {disk
            ? <Figure label={<>{t("devices:vitals.disk")} <span className="type-code text-tertiary">{disk.name}</span></>} value={t("devices:vitals.diskFree", { n: num(disk.free) })} fill={disk.used} data-vital="disk"
                sub={t("devices:vitals.diskTotal", { n: num(disk.total) })} />
            : <Figure label={t("devices:vitals.disk")} value="—" data-vital="disk" />}
        </div>
        {showFooter && (
          <div className="flex flex-wrap items-center gap-x-5 gap-y-2 border-t border-border/70 pt-3 type-footnote text-muted-foreground">
            {!v.local && (
              <span className="inline-flex items-center gap-1.5 tabular-nums" data-vital="rtt">
                <span>{t("devices:vitals.rtt")}</span>
                <strong className="font-medium text-foreground">{rtt != null ? `${rtt} ms` : "—"}</strong>
                {route && <span className="text-tertiary">· {t(`devices:route.${route}`)}</span>}
              </span>
            )}
            {tokens && (
              <span className="inline-flex items-center gap-1.5 tabular-nums" data-vital="tokens">
                <span>{t("devices:vitals.tokens")}</span>
                <strong className="font-medium text-foreground">{t("devices:vitals.tokensMinute", { n: compactTokens(tokens.per_minute) })}</strong>
                <span className="text-tertiary">· {t("devices:vitals.tokensToday", { n: compactTokens(tokens.today) })}</span>
              </span>
            )}
            {hasCost && tokens && (
              <span className="inline-flex items-center gap-1.5 tabular-nums" data-vital="cost">
                <span>{t("devices:vitals.cost")}</span>
                <strong className="font-medium text-foreground">{fmtUsd(tokens.total_cost_usd)}</strong>
                {tokens.today_cost_usd > 0 && <span className="text-tertiary">· {t("devices:vitals.costToday", { usd: fmtUsd(tokens.today_cost_usd) })}</span>}
              </span>
            )}
            {hasDiskWrites && (
              <span className="inline-flex items-center gap-1.5 tabular-nums" data-vital="disk-written">
                <span>{t("devices:vitals.diskWritten")}</span>
                <strong className="font-medium text-foreground">{fmtBytes(diskWritten)}</strong>
              </span>
            )}
            {stale && at && !offline && (
              <span className="text-attention" title={date(at, { dateStyle: "medium", timeStyle: "medium" })} data-vitals-stale>
                {t("devices:vitals.stale", { ago: agoText(at) })}
              </span>
            )}
            {vit === null && <span data-sample-state="none">{t("devices:vitals.none")}</span>}
          </div>
        )}
      </div>
    </DCard>
  )
}

/** Deeper resource breakdown on the Work & Resources tab: tokens, network throughput, and every reported disk volume. */
export function NodeResources({ v }: { v: NodeView }) {
  const t = useT(), { value: vit } = useNodeVitals(v.id)
  const l = vit?.latest
  const allDisks = disks(l)
  const net = l?.network
  const tokens = vit?.tokens
  const procDisk = l?.process?.disk_written_bytes
  const adapterDisk = l?.adapter_disk_written_bytes ?? 0
  const hasDiskWrites = procDisk != null || adapterDisk > 0
  const diskWritten = (procDisk ?? 0) + adapterDisk
  const hasCost = tokens != null && (tokens.total_cost_usd > 0 || tokens.today_cost_usd > 0)
  const sessionVitals = l?.session_vitals ?? []
  const rtt = v.linked ? rttOf(v, l) : null
  const route = linkRoute(v)
  return (
    <DCard icon={<HardDriveIcon />} title={t("devices:resources.title")} sub={t("devices:resources.sub")} data-node-resources>
      <div className="flex flex-col gap-5 px-5 pb-5">
        <div className="grid grid-cols-2 gap-x-6 gap-y-4 sm:grid-cols-3 lg:grid-cols-5">
          <Figure label={t("devices:resources.tokens")} data-resource="tokens"
            value={tokens ? t("devices:vitals.tokensMinute", { n: compactTokens(tokens.per_minute) }) : "—"}
            sub={tokens ? t("devices:vitals.tokensToday", { n: compactTokens(tokens.today) }) : t("devices:resources.noTokens")} />
          <Figure label={t("devices:resources.cost")} data-resource="cost"
            value={hasCost && tokens ? fmtUsd(tokens.total_cost_usd) : "—"}
            sub={hasCost && tokens ? t("devices:vitals.costToday", { usd: fmtUsd(tokens.today_cost_usd) }) : t("devices:resources.noCost")} />
          <Figure label={t("devices:resources.diskWritten")} data-resource="disk-written"
            value={hasDiskWrites ? fmtBytes(diskWritten) : "—"}
            sub={hasDiskWrites ? t("devices:resources.diskWrittenSub", { proc: fmtBytes(procDisk ?? 0), sessions: fmtBytes(adapterDisk) }) : t("devices:resources.noDiskWritten")} />
          <Figure label={t("devices:resources.network")} data-resource="network"
            value={net ? <span title={t("devices:vitals.receive")}>↓ {bytesPerSecond(net.rx_bytes_per_second)}</span> : "—"}
            sub={net ? <span title={t("devices:vitals.send")}>↑ {bytesPerSecond(net.tx_bytes_per_second)}</span> : t("devices:resources.noNetwork")} />
          {!v.local && (
            <Figure label={t("devices:vitals.rtt")} data-resource="rtt"
              value={rtt ?? "—"} unit={rtt != null ? " ms" : undefined}
              sub={route ? t(`devices:route.${route}`) : undefined} />
          )}
        </div>
        <div className="flex flex-col gap-2.5 border-t border-border/70 pt-4" data-node-session-vitals>
          <h3 className="type-footnote font-medium text-muted-foreground">{t("devices:resources.sessionVitals")}</h3>
          {sessionVitals.length > 0 ? (
            <div className="overflow-x-auto">
              <table className="w-full border-collapse text-left type-footnote">
                <thead>
                  <tr className="border-b border-border/70 text-muted-foreground">
                    <th className="py-1.5 pr-3 font-medium">{t("devices:resources.colSession")}</th>
                    <th className="py-1.5 px-2 text-right font-medium">{t("devices:resources.colCpu")}</th>
                    <th className="py-1.5 px-2 text-right font-medium">{t("devices:resources.colMem")}</th>
                    <th className="py-1.5 pl-2 text-right font-medium">{t("devices:resources.colDiskWritten")}</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border/50">
                  {sessionVitals.map((sv) => {
                    const sess = getSession(sv.session_id)
                    return (
                      <tr key={sv.session_id} data-node-session-vital={sv.session_id}>
                        <td className="max-w-[14rem] truncate py-2 pr-3">
                          <a href={"#/sessions/" + sv.session_id} className="text-foreground hover:text-blue hover:underline" title={sess?.title ?? sv.session_id}>
                            {sess?.title ?? sv.session_id}
                          </a>
                        </td>
                        <td className="whitespace-nowrap py-2 px-2 text-right tabular-nums text-muted-foreground">{num(one(sv.cpu_seconds))} s</td>
                        <td className="whitespace-nowrap py-2 px-2 text-right tabular-nums text-muted-foreground">{fmtBytes(sv.resident_bytes)}</td>
                        <td className="whitespace-nowrap py-2 pl-2 text-right tabular-nums font-medium text-foreground">{fmtBytes(sv.disk_written_bytes)}</td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          ) : (
            <p className="type-footnote text-muted-foreground">{t("devices:resources.noSessionVitals")}</p>
          )}
        </div>
        <div className="flex flex-col gap-2.5 border-t border-border/70 pt-4">
          <h3 className="type-footnote font-medium text-muted-foreground">{t("devices:resources.disks")}</h3>
          {allDisks.length > 0 ? (
            <ul className="grid gap-3 sm:grid-cols-2" data-resource-disks>
              {allDisks.map((d) => (
                <li key={d.root} className="flex min-w-0 flex-col gap-1.5 rounded-lg bg-muted/50 px-3.5 py-2.5">
                  <div className="flex items-baseline justify-between gap-2">
                    <span className="truncate type-code text-foreground" title={d.root}>{d.name}</span>
                    <span className="shrink-0 type-footnote tabular-nums text-muted-foreground">
                      {t("devices:vitals.diskFree", { n: num(d.free) })} / {t("devices:vitals.diskTotal", { n: num(d.total) })}
                    </span>
                  </div>
                  <Meter value={d.used} tone={fillTone(d.used) ?? "run"} />
                </li>
              ))}
            </ul>
          ) : (
            <p className="type-footnote text-muted-foreground">{t("devices:resources.noDisks")}</p>
          )}
        </div>
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
