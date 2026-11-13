/* The sessions page's activity heatmap: GitHub's contribution calendar (react-activity-calendar), darker for a busier
 * day, over 7 days, a month, a quarter or a year. Two readings of "busy": 任务, the distinct pieces of work an agent
 * turned on that day (stats metric `active`), and Token, the tokens used. One fetch per reading covers the year, so
 * switching the span asks the Hub nothing. A Hub without the stats module shows nothing here. */
import { isNotWired } from "@/api"
import { stats, type Metric } from "@/api/stats"
import { useT } from "@/i18n"
import { date, monthDay, weekday } from "@/i18n/format"
import { fmtN } from "@/stats/format"
import { Segmented } from "@/ui"
import { ActivityCalendar, type Activity } from "react-activity-calendar"
import { useEffect, useLayoutEffect, useRef, useState } from "react"
import "./sessions-heatmap.css"

type Mode = Extract<Metric, "active" | "tokens">
type Span = "7d" | "1m" | "1q" | "1y"
const SPANS: Span[] = ["7d", "1m", "1q", "1y"]
/** the days a span shows, today included */
const DAYS: Record<Span, number> = { "7d": 7, "1m": 30, "1q": 91, "1y": 365 }
const FRESH = 10 * 60_000, PREF = "steve.sessions.heat", MARGIN = 3
const pref = (() => { try { return JSON.parse(localStorage.getItem(PREF) ?? "{}") as { mode?: Mode; span?: Span } } catch { return {} } })()
let mode: Mode = pref.mode === "tokens" ? "tokens" : "active"
let span: Span = SPANS.includes(pref.span as Span) ? (pref.span as Span) : "1m"
const save = () => { try { localStorage.setItem(PREF, JSON.stringify({ mode, span })) } catch { /* private mode */ } }
/* one fetch per reading, kept while it is fresh: coming back to the list does not refetch a year */
const cache: Partial<Record<Mode, { at: number; days: Map<string, number> | null }>> = {}
let wired = true

const pad = (n: number) => String(n).padStart(2, "0")
const ymd = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
const back = (n: number) => { const d = new Date(); d.setHours(0, 0, 0, 0); d.setDate(d.getDate() - n); return d }
/** four levels by rank among the days that had anything (quartiles once there are enough of them), so a few huge
 *  days do not wash the rest out, and the busiest day is always the darkest */
function levels(values: number[]) {
  const xs = values.filter((v) => v > 0).sort((a, b) => a - b)
  return (v: number) => {
    if (v <= 0) return 0
    let at = 0; while (at < xs.length && xs[at] <= v) at++
    return Math.max(1, Math.ceil((at / xs.length) * 4))
  }
}
/* the app's own colours: they follow light and dark through the CSS variables */
const SHADES = ["color-mix(in srgb, var(--foreground) 8%, transparent)", "color-mix(in srgb, var(--blue) 30%, transparent)", "color-mix(in srgb, var(--blue) 50%, transparent)", "color-mix(in srgb, var(--blue) 75%, transparent)", "var(--blue)"]
/* Sunday first, as the calendar's labels are indexed; 2026-11-08 is a Sunday */
const weekdays = () => Array.from({ length: 7 }, (_, i) => weekday(new Date(2026, 10, 8 + i)))
const months = () => Array.from({ length: 12 }, (_, i) => date(new Date(2026, i, 1), { month: "short" }))

export function SessionsHeatmap() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const box = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(0)
  useLayoutEffect(() => {
    const el = box.current; if (!el) return
    const ro = new ResizeObserver(([e]) => setWidth(e.contentRect.width)); ro.observe(el); return () => ro.disconnect()
  }, [])
  useEffect(() => {
    const c = cache[mode]
    if (!wired || (c && Date.now() - c.at < FRESH)) return
    let live = true
    const m = mode
    stats.series({ range: "custom", from: ymd(back(DAYS["1y"])), to: ymd(back(0)) }, m, "none")
      .then((s) => { const v = s.series[0]?.values ?? []; cache[m] = { at: Date.now(), days: new Map(s.buckets.map((b, i) => [b, v[i] ?? 0])) } })
      .catch((e) => { if (isNotWired(e)) wired = false; else cache[m] = { at: Date.now() - FRESH + 60_000, days: null } })
      .finally(() => live && r())
    return () => { live = false }
  }, [mode])
  /* when a year is wider than the screen, the latest weeks show first */
  useLayoutEffect(() => { const el = box.current?.querySelector<HTMLElement>(".react-activity-calendar__scroll-container"); if (el) el.scrollLeft = el.scrollWidth }, [span, mode, width, !!cache[mode]])
  if (!wired) return null

  const days = cache[mode]?.days, failed = !!cache[mode] && !days
  const shown = Array.from({ length: DAYS[span] }, (_, i) => back(DAYS[span] - 1 - i))
  const values = shown.map((d) => days?.get(ymd(d)) ?? 0)
  const level = levels(values)
  const data: Activity[] = shown.map((d, i) => ({ date: ymd(d), count: values[i], level: level(values[i]) }))
  const active = values.filter((v) => v > 0).length, max = Math.max(0, ...values), total = values.reduce((a, b) => a + b, 0)
  const spanName = t(`views:sessions.heat.past.${span}`)
  const amount = (v: number) => (mode === "active" ? t("views:sessions.heat.dayTasks", { count: v }) : t("views:sessions.heat.dayTokens", { n: fmtN(v) }))
  const line = failed ? t("views:sessions.heat.failed") : !days ? "" : mode === "active" ? t("views:sessions.heat.sumTasks", { span: spanName, days: active, max }) : t("views:sessions.heat.sumTokens", { span: spanName, total: fmtN(total), max: fmtN(max) })
  /* a year's 53 week columns fill the width; shorter spans keep the same square size, not stretched ones */
  const block = Math.max(10, Math.min(16, Math.floor((width - 32) / 54) - MARGIN))

  return (
    <section className="mb-8" data-sessions-heatmap={mode} data-heat-span={span}>
      <div className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="type-label text-tertiary">{t("views:sessions.heat.title")}</span>
        <span className="min-w-0 flex-1 truncate type-footnote tabular-nums text-muted-foreground max-sm:order-last max-sm:basis-full" data-heat-line>{line}</span>
        <span className="flex items-center gap-2 max-sm:ml-auto">
          <Segmented size="sm" value={span} onChange={(v) => { span = v; save(); r() }} options={SPANS.map((s) => [s, t(`views:sessions.heat.span.${s}`)] as const)} label={t("views:sessions.heat.spanLabel")} />
          <Segmented size="sm" value={mode} onChange={(v) => { mode = v; save(); r() }} options={[["active", t("views:sessions.heat.tasks")], ["tokens", t("views:sessions.heat.tokens")]] as const} label={t("views:sessions.heat.title")} />
        </span>
      </div>
      <div ref={box} className="text-tertiary [&_.react-activity-calendar\_\_footer]:mt-2">
        {width > 0 && (
          <ActivityCalendar key={span} data={data} loading={!days && !failed} blockSize={block} blockMargin={MARGIN} blockRadius={3} fontSize={12}
            weekStart={1} showWeekdayLabels={["mon", "wed", "fri"]} showTotalCount={false} theme={{ light: SHADES, dark: SHADES }}
            labels={{ months: months(), weekdays: weekdays(), legend: { less: t("views:sessions.heat.less"), more: t("views:sessions.heat.more") } }}
            tooltips={{ activity: { text: (a) => { const d = new Date(a.date + "T00:00"); return `${monthDay(d)} ${weekday(d)} · ${a.count > 0 ? amount(a.count) : t("views:sessions.heat.none")}` }, hoverRestMs: 0, offset: 6 } }} />
        )}
      </div>
    </section>
  )
}
