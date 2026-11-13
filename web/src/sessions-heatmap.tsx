/* The sessions page's activity heatmap, darker for a busier day. Two readings of "busy": 任务, the distinct pieces
 * of work an agent turned on that day (stats metric `active`), and Token, the tokens used. Four spans: up to a
 * quarter, the days run left to right in one strip; a year is the familiar grid of week columns. One fetch per
 * reading covers the year, so switching the span asks the Hub nothing. A Hub without stats shows nothing here. */
import { isNotWired } from "@/api"
import { stats, type Metric } from "@/api/stats"
import { useT } from "@/i18n"
import { date, monthDay, weekday } from "@/i18n/format"
import { fmtN } from "@/stats/format"
import { Segmented } from "@/ui"
import { cn } from "cn"
import { useEffect, useLayoutEffect, useRef, useState } from "react"

type Mode = Extract<Metric, "active" | "tokens">
type Span = "7d" | "1m" | "1q" | "1y"
const SPANS: Span[] = ["7d", "1m", "1q", "1y"]
/** days a strip span shows, today included */
const STRIP: Record<Exclude<Span, "1y">, number> = { "7d": 7, "1m": 30, "1q": 91 }
const WEEKS = 52, FRESH = 10 * 60_000, PREF = "steve.sessions.heat"
const pref = (() => { try { return JSON.parse(localStorage.getItem(PREF) ?? "{}") as { mode?: Mode; span?: Span } } catch { return {} } })()
let mode: Mode = pref.mode === "tokens" ? "tokens" : "active"
let span: Span = SPANS.includes(pref.span as Span) ? (pref.span as Span) : "1m"
const save = () => { try { localStorage.setItem(PREF, JSON.stringify({ mode, span })) } catch { /* private mode */ } }
/* one fetch per reading, kept while it is fresh: coming back to the list does not refetch a year */
const cache: Partial<Record<Mode, { at: number; days: Map<string, number> | null }>> = {}
let wired = true

const pad = (n: number) => String(n).padStart(2, "0")
const ymd = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
const dayOf = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate())
const plus = (d: Date, n: number) => { const x = new Date(d); x.setDate(d.getDate() + n); return x }
/** the year's weeks, Monday first, from 51 weeks before this one to today; days after today are null */
function calendar(now = new Date()) {
  const today = dayOf(now)
  const start = plus(today, -((today.getDay() + 6) % 7) - (WEEKS - 1) * 7)
  const weeks: (Date | null)[][] = []
  for (let w = 0; w < WEEKS; w++) weeks.push(Array.from({ length: 7 }, (_, i) => { const d = plus(start, w * 7 + i); return d > today ? null : d }))
  return { weeks, from: ymd(start), to: ymd(today), today }
}
/** four steps by rank among the days that had anything (quartiles once there are enough of them), so a few huge
 *  days do not wash the rest out, and the busiest day is always the darkest */
function steps(values: number[]) {
  const xs = values.filter((v) => v > 0).sort((a, b) => a - b)
  return (v: number) => {
    if (v <= 0) return 0
    let at = 0; while (at < xs.length && xs[at] <= v) at++
    return Math.max(1, Math.ceil((at / xs.length) * 4))
  }
}
const SHADE = ["bg-foreground/[0.07]", "bg-blue/25", "bg-blue/45", "bg-blue/70", "bg-blue"]
const LABEL = "whitespace-nowrap type-footnote leading-none text-tertiary"

export function SessionsHeatmap() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [hover, setHover] = useState<string | null>(null)
  const scroller = useRef<HTMLDivElement>(null)
  const cal = calendar()
  useEffect(() => {
    const c = cache[mode]
    if (!wired || (c && Date.now() - c.at < FRESH)) return
    let live = true
    const m = mode
    stats.series({ range: "custom", from: cal.from, to: cal.to }, m, "none")
      .then((s) => { const v = s.series[0]?.values ?? []; cache[m] = { at: Date.now(), days: new Map(s.buckets.map((b, i) => [b, v[i] ?? 0])) } })
      .catch((e) => { if (isNotWired(e)) wired = false; else cache[m] = { at: Date.now() - FRESH + 60_000, days: null } })
      .finally(() => live && r())
    return () => { live = false }
  }, [mode])
  /* a phone shows the latest weeks of the year first */
  useLayoutEffect(() => { const el = scroller.current; if (el) el.scrollLeft = el.scrollWidth }, [span])
  if (!wired) return null

  const days = cache[mode]?.days, failed = !!cache[mode] && !days
  const value = (d: Date) => days?.get(ymd(d)) ?? 0
  /* the days in the span, oldest first */
  const sp = span
  const shown: Date[] = sp === "1y" ? (cal.weeks.flat().filter(Boolean) as Date[]) : Array.from({ length: STRIP[sp] }, (_, i) => plus(cal.today, i - STRIP[sp] + 1))
  const all = shown.map(value)
  const step = steps(all)
  const active = all.filter((v) => v > 0).length, max = Math.max(0, ...all), total = all.reduce((a, b) => a + b, 0)
  const spanName = t(`views:sessions.heat.past.${span}`)
  const amount = (v: number) => (mode === "active" ? t("views:sessions.heat.dayTasks", { count: v }) : t("views:sessions.heat.dayTokens", { n: fmtN(v) }))
  const line = hover
    ? (() => { const d = new Date(hover + "T00:00"), v = days?.get(hover) ?? 0; return `${monthDay(d)} ${weekday(d)} · ${v > 0 ? amount(v) : t("views:sessions.heat.none")}` })()
    : failed ? t("views:sessions.heat.failed") : !days ? "" : mode === "active" ? t("views:sessions.heat.sumTasks", { span: spanName, days: active, max }) : t("views:sessions.heat.sumTokens", { span: spanName, total: fmtN(total), max: fmtN(max) })
  const today = ymd(cal.today)
  const cell = (d: Date, className: string) => {
    const k = ymd(d), v = value(d)
    return <span key={k} onMouseEnter={() => setHover(k)} data-day={k} data-level={step(v)}
      className={cn("transition-[background-color,box-shadow] duration-150", className, days ? SHADE[step(v)] : SHADE[0], k === today && "ring-1 ring-foreground/40", hover === k && "ring-1 ring-foreground/70")} />
  }

  let body
  if (span === "1y") {
    /* a month's name over the first week that starts in it; not on the first column when the next is right behind */
    const months = cal.weeks.map((w, i) => (i === 0 || w[0]!.getMonth() !== cal.weeks[i - 1][0]!.getMonth() ? date(w[0]!, { month: "short" }) : ""))
    if (months.slice(1, 3).some(Boolean)) months[0] = ""
    body = (
      <div ref={scroller} className="max-sm:-mx-4 max-sm:overflow-x-auto max-sm:px-4 max-sm:[scrollbar-width:none]">
        <div className="grid min-w-[720px] gap-[3px]" style={{ gridTemplateColumns: `1.75rem repeat(${WEEKS}, minmax(0, 1fr))` }} onMouseLeave={() => setHover(null)} role="img" aria-label={line}>
          <span />
          {months.map((m, i) => <span key={"m" + i} className={cn("h-4 overflow-visible", LABEL)}>{m}</span>)}
          {Array.from({ length: 7 }, (_, row) => [
            <span key={"w" + row} className="relative">{row % 2 === 0 && <span className={cn("absolute inset-y-0 left-0 flex items-center", LABEL)}>{weekday(cal.weeks[0][row]!)}</span>}</span>,
            ...cal.weeks.map((w, col) => (w[row] ? cell(w[row]!, "aspect-square rounded-[3px]") : <span key={col + "-" + row} />)),
          ])}
        </div>
      </div>
    )
  } else {
    /* one row of days; a week names each day, a month its Mondays, a quarter its months */
    const label = (d: Date, i: number) =>
      span === "7d" ? `${weekday(d)} ${d.getMonth() + 1}/${d.getDate()}`
      : span === "1m" ? (d.getDay() === 1 ? `${d.getMonth() + 1}/${d.getDate()}` : "")
      : i === 0 || d.getDate() === 1 ? date(d, { month: "short" }) : ""
    const cols = { gridTemplateColumns: `repeat(${shown.length}, minmax(0, 1fr))` }
    const gap = span === "1q" ? "gap-[2px]" : span === "1m" ? "gap-[3px]" : "gap-1"
    body = (
      <div onMouseLeave={() => setHover(null)} role="img" aria-label={line}>
        <div className={cn("grid", gap)} style={cols}>{shown.map((d) => cell(d, cn("h-10", span === "1q" ? "rounded-[2px]" : "rounded-[4px]")))}</div>
        <div className={cn("mt-2 grid", gap)} style={cols}>{shown.map((d, i) => <span key={"l" + i} className={cn("h-4 overflow-visible", LABEL, span === "7d" && "text-center")}>{label(d, i)}</span>)}</div>
      </div>
    )
  }

  return (
    <section className="mb-8" data-sessions-heatmap={mode} data-heat-span={span}>
      <div className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="type-label text-tertiary">{t("views:sessions.heat.title")}</span>
        <span className={cn("min-w-0 flex-1 truncate type-footnote tabular-nums max-sm:order-last max-sm:basis-full", hover ? "text-foreground/80" : "text-muted-foreground")} aria-live="polite" data-heat-line>{line}</span>
        <span className="flex items-center gap-2 max-sm:ml-auto">
          <Segmented size="sm" value={span} onChange={(v) => { span = v; save(); setHover(null); r() }} options={SPANS.map((s) => [s, t(`views:sessions.heat.span.${s}`)] as const)} label={t("views:sessions.heat.spanLabel")} />
          <Segmented size="sm" value={mode} onChange={(v) => { mode = v; save(); setHover(null); r() }} options={[["active", t("views:sessions.heat.tasks")], ["tokens", t("views:sessions.heat.tokens")]] as const} label={t("views:sessions.heat.title")} />
        </span>
      </div>
      {body}
      <div className="mt-2 flex items-center justify-end gap-1 type-footnote text-tertiary">
        <span className="mr-1">{t("views:sessions.heat.less")}</span>
        {SHADE.map((s) => <span key={s} className={cn("size-2.5 rounded-[2px]", s)} />)}
        <span className="ml-1">{t("views:sessions.heat.more")}</span>
      </div>
    </section>
  )
}
