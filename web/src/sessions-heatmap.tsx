/* The sessions page's activity heatmap: a year of days in week columns, darker for a busier day. Two readings of
 * "busy": 任务, the distinct pieces of work an agent turned on that day (stats metric `active`), and Token, the
 * tokens used. A Hub without the stats module shows nothing here. */
import { isNotWired } from "@/api"
import { stats, type Metric } from "@/api/stats"
import { useT } from "@/i18n"
import { date, monthDay, weekday } from "@/i18n/format"
import { fmtN } from "@/stats/format"
import { Segmented } from "@/ui"
import { cn } from "cn"
import { useEffect, useLayoutEffect, useRef, useState } from "react"

type Mode = Extract<Metric, "active" | "tokens">
const WEEKS = 52, FRESH = 10 * 60_000
let mode: Mode = "active"
/* one fetch per reading, kept while it is fresh: coming back to the list does not refetch a year */
const cache: Partial<Record<Mode, { at: number; days: Map<string, number> | null }>> = {}
let wired = true

const pad = (n: number) => String(n).padStart(2, "0")
const ymd = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
/** the weeks, Monday first, from 51 weeks before this one to today; days after today are null */
function calendar(now = new Date()) {
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate())
  const start = new Date(today); start.setDate(today.getDate() - ((today.getDay() + 6) % 7) - (WEEKS - 1) * 7)
  const weeks: (Date | null)[][] = []
  for (let w = 0; w < WEEKS; w++) weeks.push(Array.from({ length: 7 }, (_, i) => { const d = new Date(start); d.setDate(start.getDate() + w * 7 + i); return d > today ? null : d }))
  return { weeks, from: ymd(start), to: ymd(today), today: ymd(today) }
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
  /* a phone shows the latest weeks first */
  useLayoutEffect(() => { const el = scroller.current; if (el) el.scrollLeft = el.scrollWidth }, [])
  if (!wired) return null

  const days = cache[mode]?.days, failed = cache[mode] && !days
  const value = (d: string) => days?.get(d) ?? 0
  const all = cal.weeks.flat().filter(Boolean).map((d) => value(ymd(d!)))
  const step = steps(all)
  const active = all.filter((v) => v > 0).length, max = Math.max(0, ...all), total = all.reduce((a, b) => a + b, 0)
  const amount = (v: number) => (mode === "active" ? t("views:sessions.heat.dayTasks", { count: v }) : t("views:sessions.heat.dayTokens", { n: fmtN(v) }))
  const line = hover
    ? (() => { const d = new Date(hover + "T00:00"); const v = value(hover); return `${monthDay(d)} ${weekday(d)} · ${v > 0 ? amount(v) : t("views:sessions.heat.none")}` })()
    : failed ? t("views:sessions.heat.failed") : !days ? "" : mode === "active" ? t("views:sessions.heat.sumTasks", { days: active, max }) : t("views:sessions.heat.sumTokens", { total: fmtN(total), max: fmtN(max) })
  /* a month's name over the first week that starts in it */
  const months = cal.weeks.map((w, i) => { const d = w[0]!; return i === 0 || d.getMonth() !== cal.weeks[i - 1][0]!.getMonth() ? date(d, { month: "short" }) : "" })
  /* the first column's month only when the next one is not right behind it */
  if (months.slice(1, 3).some(Boolean)) months[0] = ""

  return (
    <section className="mb-8" data-sessions-heatmap={mode}>
      <div className="mb-3 flex min-h-7 items-center gap-3">
        <span className="type-label text-tertiary">{t("views:sessions.heat.title")}</span>
        <span className={cn("min-w-0 flex-1 truncate type-footnote tabular-nums", hover ? "text-foreground/80" : "text-muted-foreground")} aria-live="polite" data-heat-line>{line}</span>
        <Segmented size="sm" value={mode} onChange={(v) => { mode = v; setHover(null); r() }} options={[["active", t("views:sessions.heat.tasks")], ["tokens", t("views:sessions.heat.tokens")]] as const} label={t("views:sessions.heat.title")} />
      </div>
      <div ref={scroller} className="max-sm:-mx-4 max-sm:overflow-x-auto max-sm:px-4 max-sm:[scrollbar-width:none]">
        <div className="grid min-w-[720px] gap-[3px]" style={{ gridTemplateColumns: `1.75rem repeat(${WEEKS}, minmax(0, 1fr))` }} onMouseLeave={() => setHover(null)} role="img" aria-label={line}>
          <span />
          {months.map((m, i) => <span key={"m" + i} className="h-4 overflow-visible whitespace-nowrap type-footnote leading-none text-tertiary">{m}</span>)}
          {Array.from({ length: 7 }, (_, row) => [
            <span key={"w" + row} className="relative">{row % 2 === 0 && <span className="absolute inset-y-0 left-0 flex items-center whitespace-nowrap type-footnote leading-none text-tertiary">{weekday(cal.weeks[0][row]!)}</span>}</span>,
            ...cal.weeks.map((w, col) => {
              const d = w[row]
              if (!d) return <span key={col + "-" + row} />
              const k = ymd(d), v = value(k)
              return <span key={k} onMouseEnter={() => setHover(k)} data-day={k} data-level={step(v)}
                className={cn("aspect-square rounded-[3px] transition-[background-color,box-shadow] duration-150", days ? SHADE[step(v)] : SHADE[0], k === cal.today && "ring-1 ring-foreground/40", hover === k && "ring-1 ring-foreground/70")} />
            }),
          ])}
        </div>
      </div>
      <div className="mt-2 flex items-center justify-end gap-1 type-footnote leading-none text-tertiary">
        <span className="mr-1">{t("views:sessions.heat.less")}</span>
        {SHADE.map((s) => <span key={s} className={cn("size-2.5 rounded-[2px]", s)} />)}
        <span className="ml-1">{t("views:sessions.heat.more")}</span>
      </div>
    </section>
  )
}
