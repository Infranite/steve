/* The sessions page's activity: how much work happened each day lately. Two readings: 任务, the distinct pieces of work
 * an agent turned on (stats metric `active`), and Token, the tokens used.
 *   7 天 / 1 个月 / 3 个月   a bar per day, the stats page's own chart
 *   1 年                     GitHub's contribution calendar (react-activity-calendar), a column per week
 * The year's daily figures are fetched once per reading and serve every span. A Hub without stats shows nothing. */
import { isNotWired } from "@/api"
import { stats, type Metric } from "@/api/stats"
import { useT } from "@/i18n"
import { date, monthDay, weekday } from "@/i18n/format"
import { fmtN } from "@/stats/format"
import { Segmented } from "@/ui"
import { Chart } from "@/stats/chart"
import { cn } from "cn"
import { ActivityCalendar, type Activity } from "react-activity-calendar"
import { useEffect, useLayoutEffect, useRef, useState } from "react"
import "./sessions-heatmap.css"

type Mode = Extract<Metric, "active" | "tokens">
type Span = "7d" | "1m" | "1q" | "1y"
const SPANS: Span[] = ["7d", "1m", "1q", "1y"]
/** the days a span covers, today included */
const DAYS: Record<Span, number> = { "7d": 7, "1m": 30, "1q": 91, "1y": 365 }
const FRESH = 10 * 60_000, PREF = "steve.sessions.heat"
const pref = (() => { try { return JSON.parse(localStorage.getItem(PREF) ?? "{}") as { mode?: Mode; span?: Span } } catch { return {} } })()
let mode: Mode = pref.mode === "tokens" ? "tokens" : "active"
let span: Span = SPANS.includes(pref.span as Span) ? (pref.span as Span) : "1m"
const save = () => { try { localStorage.setItem(PREF, JSON.stringify({ mode, span })) } catch { /* private mode */ } }
/* per reading: the year by day, which serves every span */
const daily: Partial<Record<Mode, { at: number; days: Map<string, number> | null }>> = {}
let wired = true

const pad = (n: number) => String(n).padStart(2, "0")
const ymd = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
const back = (n: number) => { const d = new Date(); d.setHours(0, 0, 0, 0); d.setDate(d.getDate() - n); return d }
const fresh = (c?: { at: number }) => !!c && Date.now() - c.at < FRESH
/** four levels by rank among the cells that had anything (quartiles once there are enough of them), so a few huge
 *  cells do not wash the rest out, and the busiest is always the darkest */
function levels(values: number[]) {
  const xs = values.filter((v) => v > 0).sort((a, b) => a - b)
  return (v: number) => {
    if (v <= 0) return 0
    let at = 0; while (at < xs.length && xs[at] <= v) at++
    return Math.max(1, Math.ceil((at / xs.length) * 4))
  }
}
/* the app's accent at five strengths for the year's calendar, as CSS so it follows the theme by itself */
const ALPHA = [0, 0.3, 0.5, 0.75, 1]
const CSS_SHADES = ["color-mix(in srgb, var(--foreground) 8%, transparent)", ...ALPHA.slice(1).map((a) => `color-mix(in srgb, var(--blue) ${a * 100}%, transparent)`)]
/** re-render when the theme (the `dark` class) changes, so the grid's colours follow */
function useThemeTick() {
  const [, f] = useState(0)
  useEffect(() => { const mo = new MutationObserver(() => f((x) => x + 1)); mo.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] }); return () => mo.disconnect() }, [])
}


export function SessionsHeatmap() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  useThemeTick()
  const box = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(0)
  useLayoutEffect(() => { const el = box.current; if (!el) return; const ro = new ResizeObserver(([e]) => setWidth(e.contentRect.width)); ro.observe(el); return () => ro.disconnect() }, [])
  const sp = span, m = mode
  useEffect(() => {
    if (!wired) return
    let live = true
    const fail = (e: unknown) => { if (isNotWired(e)) wired = false; return null }
    if (!fresh(daily[m])) {
      stats.series({ range: "custom", from: ymd(back(DAYS["1y"])), to: ymd(back(0)) }, m, "none")
        .then((s) => { const v = s.series[0]?.values ?? []; daily[m] = { at: Date.now(), days: new Map(s.buckets.map((b, i) => [b, v[i] ?? 0])) } })
        .catch((e) => { fail(e); daily[m] = { at: Date.now() - FRESH + 60_000, days: null } })
        .finally(() => live && r())
    }
    return () => { live = false }
  }, [sp, m])
  /* when a year is wider than the screen, the latest weeks show first */
  useLayoutEffect(() => { const el = box.current?.querySelector<HTMLElement>(".react-activity-calendar__scroll-container"); if (el) el.scrollLeft = el.scrollWidth })
  if (!wired) return null

  const amount = (v: number) => (m === "active" ? t("views:sessions.heat.dayTasks", { count: v }) : t("views:sessions.heat.dayTokens", { n: fmtN(v) }))
  const dayName = (d: Date) => `${monthDay(d)} ${weekday(d)}`
  const spanName = t(`views:sessions.heat.past.${sp}`)
  /* the figures of the span, whatever its layout: per day */
  const days = daily[m]?.days
  const loaded = !!days, failed = !!daily[m] && !days
  const span_days = Array.from({ length: DAYS[sp] }, (_, i) => back(DAYS[sp] - 1 - i))
  const perDay = span_days.map((d) => days?.get(ymd(d)) ?? 0)
  const active = perDay.filter((v) => v > 0).length, total = perDay.reduce((a, b) => a + b, 0)
  const peak = Math.max(0, ...perDay)
  const line = failed ? t("views:sessions.heat.failed") : !loaded ? "" : m === "active" ? t("views:sessions.heat.sumTasks", { span: spanName, days: active, max: peak }) : t("views:sessions.heat.sumTokens", { span: spanName, total: fmtN(total), max: fmtN(peak) })

  let grid
  if (sp === "1y") {
    const level = levels(perDay)
    const data: Activity[] = span_days.map((d, i) => ({ date: ymd(d), count: perDay[i], level: level(perDay[i]) }))
    const block = Math.max(9, Math.min(14, Math.floor(((width || 1000) - 40) / 54) - 3))
    grid = (
      <ActivityCalendar data={data} loading={!loaded && !failed} blockSize={block} blockMargin={3} blockRadius={3} fontSize={12}
        weekStart={1} showWeekdayLabels={["mon", "wed", "fri"]} showTotalCount={false} showColorLegend={false} theme={{ light: CSS_SHADES, dark: CSS_SHADES }}
        labels={{ months: Array.from({ length: 12 }, (_, i) => date(new Date(2026, i, 1), { month: "short" })), weekdays: Array.from({ length: 7 }, (_, i) => weekday(new Date(2026, 10, 8 + i))) }}
        tooltips={{ activity: { text: (a) => `${dayName(new Date(a.date + "T00:00"))} · ${a.count > 0 ? amount(a.count) : t("views:sessions.heat.none")}`, hoverRestMs: 0, offset: 6 } }} />
    )
  } else {
    /* a bar per day; the accent comes from the theme, read here because the chart draws SVG */
    const blue = getComputedStyle(document.documentElement).getPropertyValue("--blue").trim() || "#3e63dd"
    const data = { bucket: "day" as const, buckets: span_days.map(ymd), series: [{ key: m, label: m === "active" ? t("views:sessions.heat.tasks") : t("views:sessions.heat.tokens"), values: span_days.map((_, i) => (loaded ? perDay[i] : 0)) }] }
    grid = <Chart data={data} kind="stack" colors={[blue]} int={m === "active"} height={sp === "7d" ? 96 : 120} label={line || t("views:sessions.heat.title")}
      fmt={(v) => (m === "active" ? String(Math.round(v)) : fmtN(v))} tipFmt={(v) => (v > 0 ? amount(v) : t("views:sessions.heat.none"))} />
  }

  return (
    <section className="mb-8" data-sessions-heatmap={m} data-heat-span={sp}>
      <div className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="type-label text-muted-foreground">{t("views:sessions.heat.title")}</span>
        <span className="min-w-0 flex-1 truncate type-footnote tabular-nums text-muted-foreground max-sm:order-last max-sm:basis-full" data-heat-line>{line}</span>
        <span className="flex items-center gap-2 max-sm:ml-auto">
          <Segmented size="sm" value={sp} onChange={(v) => { span = v; save(); r() }} options={SPANS.map((s) => [s, t(`views:sessions.heat.span.${s}`)] as const)} label={t("views:sessions.heat.spanLabel")} />
          <Segmented size="sm" value={m} onChange={(v) => { mode = v; save(); r() }} options={[["active", t("views:sessions.heat.tasks")], ["tokens", t("views:sessions.heat.tokens")]] as const} label={t("views:sessions.heat.title")} />
        </span>
      </div>
      <div ref={box} className={cn("text-tertiary", !loaded && !failed && "animate-pulse")}>{grid}</div>
      {sp === "1y" && <div className="mt-2 flex items-center justify-end gap-1 type-footnote text-tertiary">
        <span className="mr-1">{t("views:sessions.heat.less")}</span>
        {CSS_SHADES.map((s) => <span key={s} className="size-2.5 rounded-[2px]" style={{ background: s }} />)}
        <span className="ml-1">{t("views:sessions.heat.more")}</span>
      </div>}
    </section>
  )
}
