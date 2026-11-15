/** Trailing-window readings come only from /stats/realtime. Never derive TPS from summary totals,
 * item status, or a previous filter's readings. The server owns windowing and session deduplication. */
import { useEffect, useId, useState } from "react"
import { InfoIcon } from "lucide-react"
import { stats, type Realtime, type RealtimeFilter, type RealtimePoint, type RealtimeState } from "@/api/stats"
import { Badge, type BadgeTone } from "@/components/affect/badge-clean"
import { KpiChart } from "@/components/affect/kpi-chart-clean"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { useT } from "@/i18n"
import { date, num } from "@/i18n/format"
import "./realtime.css"

const STEP = 1000, STALE = 5000, TIMEOUT = 4000
const states: RealtimeState[] = ["measured", "estimated", "idle", "unavailable", "partial"]
const tones: Record<RealtimeState, BadgeTone> = { measured: "success", estimated: "info", idle: "neutral", unavailable: "danger", partial: "warning" }
const colors: Record<RealtimeState, string> = { measured: "var(--success)", estimated: "var(--blue)", idle: "var(--muted-foreground)", partial: "var(--attention)", unavailable: "var(--tertiary)" }
type Problem = "loading" | "request" | "invalid" | null
type Reading = { data: Realtime | null; problem: Problem }

const object = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === "object"
const timestamp = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && v > 0 && Number.isFinite(new Date(v).getTime())
const nonnegative = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0
const count = (v: unknown): v is number => nonnegative(v) && Number.isInteger(v)
const point = (v: unknown) => object(v) && timestamp(v.at) && (v.tps === null || nonnegative(v.tps)) && states.includes(v.state as RealtimeState)
/** HTML fallbacks, older interfaces and malformed payloads must not become zeros. Extra fields are allowed. */
function snapshot(v: unknown): Realtime | null {
  if (!object(v) || !point(v) || v.window_ms !== 10000 || v.step_ms !== STEP ||
    !(v.output_sessions === null || count(v.output_sessions)) || !object(v.coverage) ||
    ![v.coverage.measured, v.coverage.estimated, v.coverage.unavailable].every(count) ||
    !Array.isArray(v.series) || !v.series.every(point)) return null
  return v as Realtime
}

function useRealtime({ project, bot }: RealtimeFilter) {
  const [reading, setReading] = useState<Reading>({ data: null, problem: "loading" })
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    let disposed = false, active: AbortController | null = null
    const load = async () => {
      // A slow request never overlaps the next one; it times out so polling can recover.
      if (active) return
      const controller = new AbortController()
      active = controller
      const timeout = setTimeout(() => controller.abort(), TIMEOUT)
      try {
        const data = snapshot(await stats.realtime({ project, bot }, controller.signal))
        if (!disposed) {
          setNow(Date.now())
          setReading({ data, problem: data ? null : "invalid" })
        }
      } catch {
        if (!disposed) setReading({ data: null, problem: "request" })
      } finally {
        clearTimeout(timeout)
        active = null
      }
    }
    void load()
    const timer = setInterval(() => { setNow(Date.now()); void load() }, STEP)
    return () => { disposed = true; clearInterval(timer); active?.abort() }
  }, [project, bot])
  return { ...reading, now }
}

/** Sixty timestamped slots, including holes omitted by the server. Never connect across an absent second.
 * Each value is the backend's trailing-10s TPS at that second, not output generated in that one second. */
function trend(data: Realtime | null, now: number): RealtimePoint[] {
  const end = Math.floor((data?.at ?? now) / STEP) * STEP
  const byTime = new Map<number, RealtimePoint>()
  for (const p of data?.series ?? []) {
    const bucket = Math.floor(p.at / STEP) * STEP
    if (!byTime.has(bucket) || byTime.get(bucket)!.at < p.at) byTime.set(bucket, p)
  }
  return Array.from({ length: 60 }, (_, i) => {
    const at = end - (59 - i) * STEP, p = byTime.get(at)
    return { at, tps: p && p.state !== "unavailable" && p.state !== "partial" ? p.tps : null, state: p?.state ?? "unavailable" }
  })
}

/** Keyed by project/Bot at the page boundary: old values disappear in the same render as a filter change,
 * while cleanup aborts its request and ignores any late answer. History filters never remount this component. */
export function RealtimeStats(filter: RealtimeFilter) {
  const t = useT(), id = useId()
  const { data, problem, now } = useRealtime(filter)
  // A repeated cached timestamp ages out even when the HTTP requests themselves keep succeeding.
  const stale = data !== null && Math.abs(now - data.at) > STALE
  const current = !problem && !stale ? data : null
  const state = current?.state ?? "unavailable"
  const reason = problem ?? (stale ? "stale" : state)
  const points = trend(current, now)
  const number = (n: number) => num(n, { maximumSignificantDigits: 3 })
  // Partial coverage has unknown totals. Optional observed subtotals are deliberately not used
  // as TPS or session totals, including when only one session has stale collection evidence.
  const unknown = state === "unavailable" || state === "partial"
  const rate = unknown ? null : current?.tps
  const sessions = unknown ? null : current?.output_sessions
  const estimated = state === "estimated"
  const value = rate == null ? "—" : (estimated ? "≈ " : "") + number(rate)
  const clock = (at: number) => date(at, { hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23" })
  const label = (s: RealtimeState) => t(`stats:realtime.states.${s}`)
  const title = problem === "loading" ? t("stats:realtime.loading") : label(state)
  const series = states.filter((s) => s !== "unavailable").map((s) => ({ label: label(s), color: colors[s], values: points.map((p) => p.state === s ? p.tps : null) }))
  return (
    <section className="stats-realtime flex min-w-0 flex-col gap-3" aria-labelledby={id} data-realtime data-realtime-state={state} data-realtime-reason={reason}>
      <header className="flex flex-wrap items-center gap-2">
        <h2 id={id} className="type-callout font-medium">{t("stats:realtime.title")}</h2>
        {/* Only status changes are announced, not every one-second number update. */}
        <span role="status"><Badge tone={problem === "loading" ? "neutral" : tones[state]} dot>{title}</Badge></span>
        <span className="type-footnote text-muted-foreground">{t("stats:realtime.scope")}</span>
        <Tooltip>
          <TooltipTrigger render={<button type="button" aria-label={t("stats:realtime.sessionsHelp")} className="inline-flex size-6 items-center justify-center rounded focus-visible:outline-2 focus-visible:outline-ring" />}><InfoIcon className="size-3.5" /></TooltipTrigger>
          <TooltipContent className="max-w-72">{t("stats:realtime.tpsDef")} {t("stats:realtime.sessionsDef")} {t("stats:realtime.collectionDef")}</TooltipContent>
        </Tooltip>
      </header>
      <p className="type-footnote text-muted-foreground" data-realtime-message>{t(`stats:realtime.messages.${reason}`)}</p>
      <KpiChart title={t("stats:realtime.tps")} value={value} series={series} format={number}
          labels={points.map((_, i) => i === 59 ? t("stats:realtime.now") : t("stats:realtime.secondsAgo", { count: 59 - i }))}
          tooltipLabels={points.map((p) => `${clock(p.at)} · ${label(p.state)}`)}
          emptyText={t("stats:realtime.noTrend")} missingText={t("stats:realtime.missing")}
          chartLabel={t("stats:realtime.chartAria")} footer={t("stats:realtime.trendDef")}
          metricAside={<dl className="min-w-0">
            <dt className="type-callout font-medium">{t("stats:realtime.sessions")}</dt>
            <dd className="ui-kpic__value" data-realtime-sessions>{sessions == null ? "—" : num(sessions)}</dd>
            <dd className="mt-1 type-footnote text-muted-foreground">{t(state === "partial" ? "stats:realtime.sessionsPartialWindow" : "stats:realtime.sessionsWindow")}</dd>
          </dl>} />
      <div className="flex flex-wrap gap-x-4 gap-y-1">
        {current && <p className="type-footnote text-muted-foreground" data-realtime-coverage>{t("stats:realtime.coverage", current.coverage)}</p>}
        {data && <p className="mt-auto type-footnote text-muted-foreground" data-realtime-at>{t("stats:realtime.updated", { time: clock(data.at) })}</p>}
      </div>
      <div className="flex flex-wrap gap-x-3 gap-y-1 type-footnote text-muted-foreground" aria-label={t("stats:realtime.legend")}>
        {states.map((s) => <span key={s} className="inline-flex items-center gap-1"><span aria-hidden className="size-1.5 rounded-full" style={{ background: colors[s] }} />{label(s)}</span>)}
      </div>
      <details className="type-footnote text-muted-foreground" data-realtime-table>
        <summary className="w-fit cursor-pointer rounded focus-visible:outline-2 focus-visible:outline-ring">{t("stats:realtime.details")}</summary>
        <div className="mt-2 max-h-60 overflow-y-auto">
          <table className="w-full text-left tabular-nums">
            <caption className="sr-only">{t("stats:realtime.trendDef")}</caption>
            <thead><tr><th scope="col">{t("stats:realtime.time")}</th><th scope="col">TPS</th><th scope="col">{t("stats:realtime.status")}</th></tr></thead>
            <tbody>{points.map((p) => <tr key={p.at}><th scope="row" className="font-normal">{clock(p.at)}</th><td>{p.tps === null ? t("stats:realtime.missing") : number(p.tps)}</td><td>{label(p.state)}</td></tr>)}</tbody>
          </table>
        </div>
      </details>
    </section>
  )
}
