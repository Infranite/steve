/* The sessions page's activity heatmap, darker for busier. Two readings: 任务, the distinct pieces of work an agent
 * turned on (stats metric `active`), and Token, the tokens used. Each span is laid out so its cells fill the width:
 *   7 天          a row per day, a cell per hour (the hourly series of each day)
 *   1 / 3 个月    a row per month, a cell per day of the month
 *   1 年          GitHub's contribution calendar, a column per week
 * The grids are open-source components: @nivo/heatmap for the rows, react-activity-calendar for the year. The year's
 * daily figures are fetched once per reading and serve every span's summary. A Hub without stats shows nothing. */
import { isNotWired } from "@/api"
import { stats, type Metric } from "@/api/stats"
import { useT } from "@/i18n"
import { date, monthDay, weekday } from "@/i18n/format"
import { fmtN } from "@/stats/format"
import { Segmented } from "@/ui"
import { ResponsiveHeatMap } from "@nivo/heatmap"
import { cn } from "cn"
import { ActivityCalendar, type Activity } from "react-activity-calendar"
import { useEffect, useLayoutEffect, useRef, useState } from "react"
import "./sessions-heatmap.css"

type Mode = Extract<Metric, "active" | "tokens">
type Span = "7d" | "1m" | "1q" | "1y"
const SPANS: Span[] = ["7d", "1m", "1q", "1y"]
/** the days a span covers, today included */
const DAYS: Record<Span, number> = { "7d": 7, "1m": 30, "1q": 91, "1y": 365 }
const FRESH = 10 * 60_000, PREF = "steve.sessions.heat", ROW = 26
const pref = (() => { try { return JSON.parse(localStorage.getItem(PREF) ?? "{}") as { mode?: Mode; span?: Span } } catch { return {} } })()
let mode: Mode = pref.mode === "tokens" ? "tokens" : "active"
let span: Span = SPANS.includes(pref.span as Span) ? (pref.span as Span) : "1m"
const save = () => { try { localStorage.setItem(PREF, JSON.stringify({ mode, span })) } catch { /* private mode */ } }
/* per reading: the year by day (every span's summary), and the last 7 days by hour (null = an hour not yet come) */
const daily: Partial<Record<Mode, { at: number; days: Map<string, number> | null }>> = {}
const hourly: Partial<Record<Mode, { at: number; rows: Map<string, (number | null)[]> | null }>> = {}
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
/* the app's accent at five strengths; the calendar takes CSS (it follows the theme by itself), the SVG grid concrete
 * colours read from the same variables */
const ALPHA = [0, 0.3, 0.5, 0.75, 1]
const CSS_SHADES = ["color-mix(in srgb, var(--foreground) 8%, transparent)", ...ALPHA.slice(1).map((a) => `color-mix(in srgb, var(--blue) ${a * 100}%, transparent)`)]
function shades() {
  const css = getComputedStyle(document.documentElement)
  const rgb = (v: string) => { const h = css.getPropertyValue(v).trim().replace("#", ""); return h.length === 6 ? [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16)).join(", ") : "128, 128, 128" }
  const fg = rgb("--foreground"), blue = rgb("--blue")
  return [`rgba(${fg}, 0.08)`, ...ALPHA.slice(1).map((a) => `rgba(${blue}, ${a})`)]
}
/** re-render when the theme (the `dark` class) changes, so the grid's colours follow */
function useThemeTick() {
  const [, f] = useState(0)
  useEffect(() => { const mo = new MutationObserver(() => f((x) => x + 1)); mo.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] }); return () => mo.disconnect() }, [])
}

type Cell = { x: string; y: number | null; day: string; hour?: number }
const Tip = ({ text }: { text: string }) => <div className="react-activity-calendar__tooltip">{text}</div>

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
    if (sp === "7d" && !fresh(hourly[m])) {
      const days = Array.from({ length: 7 }, (_, i) => ymd(back(6 - i)))
      Promise.all(days.map((d) => stats.series({ range: "custom", from: d, to: d }, m, "none", true).then((s) => [d, s.series[0]?.values ?? []] as const)))
        .then((xs) => { hourly[m] = { at: Date.now(), rows: new Map(xs) } })
        .catch((e) => { fail(e); hourly[m] = { at: Date.now() - FRESH + 60_000, rows: null } })
        .finally(() => live && r())
    }
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
  const days = daily[m]?.days, hours = hourly[m]?.rows
  const loaded = !!days && (sp !== "7d" || !!hours), failed = (!!daily[m] && !days) || (sp === "7d" && !!hourly[m] && !hours)
  const span_days = Array.from({ length: DAYS[sp] }, (_, i) => back(DAYS[sp] - 1 - i))
  /* a day's figure comes from the daily series also for 7 天: an item worked on in three hours is one task that day */
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
    /* rows and cells of the grid: 7 天 a row per day and a cell per hour; a month or a quarter a row per month and a
     * cell per day of it (days outside the span, or a month's missing 29th–31st, are left blank) */
    let rows: { id: string; data: Cell[] }[]
    let ticks: string[]
    if (sp === "7d") {
      rows = span_days.map((d) => { const k = ymd(d), v = hours?.get(k) ?? []; return { id: `${weekday(d)} ${d.getMonth() + 1}/${d.getDate()}`, data: Array.from({ length: 24 }, (_, h) => ({ x: pad(h) + ":00", y: loaded ? (v[h] ?? null) : 0, day: k, hour: h })) } })
      ticks = (width && width < 640 ? [0, 6, 12, 18] : [0, 3, 6, 9, 12, 15, 18, 21]).map((h) => pad(h) + ":00")
    } else {
      const first = span_days[0], inSpan = new Map(span_days.map((d, i) => [ymd(d), perDay[i]]))
      rows = []
      for (let y = first.getFullYear(), mo = first.getMonth(); new Date(y, mo, 1) <= span_days[span_days.length - 1]; mo++) {
        const head = new Date(y, mo, 1)
        rows.push({ id: date(head, { month: "short" }), data: Array.from({ length: 31 }, (_, i) => { const d = new Date(y, mo, i + 1), k = ymd(d), ok = d.getMonth() === head.getMonth() && inSpan.has(k); return { x: String(i + 1), y: ok ? inSpan.get(k)! : null, day: k } }) })
      }
      ticks = width && width < 640 ? ["1", "10", "20", "31"] : ["1", "5", "10", "15", "20", "25", "31"]
    }
    const level = levels(rows.flatMap((x) => x.data.map((c) => c.y ?? 0)))
    const shade = shades()
    grid = (
      <div style={{ height: rows.length * ROW + 22 }} role="img" aria-label={line}>
        <ResponsiveHeatMap<Cell, object> data={rows} animate={false} margin={{ top: 22, right: 0, bottom: 0, left: sp === "7d" ? 76 : 44 }}
          xInnerPadding={0.12} yInnerPadding={0.12} borderRadius={3} borderWidth={0} enableLabels={false} emptyColor="transparent"
          colors={(c) => shade[loaded ? level(c.value ?? 0) : 0]} activeOpacity={1} inactiveOpacity={1} hoverTarget="cell"
          axisTop={{ tickSize: 0, tickPadding: 6, tickValues: ticks }} axisLeft={{ tickSize: 0, tickPadding: 10 }}
          theme={{ text: { fontSize: 12, fill: "var(--tertiary)", fontFamily: "inherit" }, axis: { ticks: { text: { fontSize: 12, fill: "var(--tertiary)" } } } }}
          tooltip={({ cell }) => cell.value == null ? null : <Tip text={`${dayName(new Date(cell.data.day + "T00:00"))}${cell.data.hour != null ? ` ${pad(cell.data.hour)}:00–${pad(cell.data.hour + 1)}:00` : ""} · ${cell.value > 0 ? amount(cell.value) : t("views:sessions.heat.none")}`} />} />
      </div>
    )
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
      <div className="mt-2 flex items-center justify-end gap-1 type-footnote text-tertiary">
        <span className="mr-1">{t("views:sessions.heat.less")}</span>
        {CSS_SHADES.map((s) => <span key={s} className="size-2.5 rounded-[2px]" style={{ background: s }} />)}
        <span className="ml-1">{t("views:sessions.heat.more")}</span>
      </div>
    </section>
  )
}
