/* 日历's building blocks: the timeline of one or more days (day, 3 日, week) with its all-day row, the agenda list, the
 * week strip of a phone, the month grid and the sources list of the week view's rail. affectUI has no calendar grid or
 * week view, so these are Steve's own, drawn with its tokens: each calendar in its colour (the source's, else a series
 * colour), an invitation you have not answered dashed, a conflict with a red edge, a declined meeting faded, the
 * current time as a red line. */
import { useState, type CSSProperties } from "react"
import { ChevronLeftIcon, ChevronRightIcon, FlagIcon, SquareIcon } from "lucide-react"
import { cn } from "cn"
import { Checkbox } from "@/components/ui/checkbox"
import { useT } from "@/i18n"
import { ago } from "@/i18n/format"
import type { CalSource } from "@/api/calendar"
import type { Todo } from "@/todos/model"
import { daysLabel, dayLong, monthLabel, weekdayLabel, weekdayNarrow } from "./format"
import {
  awaitsReply, dayIn, declined, hmIn, hoursOf, layoutDay, marksOn, monthGrid, onDay, plusDays, zoned,
  type CalEvent, type DayMark, type Placed,
} from "./model"

/** where the page is and how to get elsewhere: the links of a day, a view, an event */
export type Nav = { day: (d: string) => string; event: (id: string) => string; selected?: string }
/** the colour of each calendar by id */
export type Colors = Map<string, string>
const colorFor = (colors: Colors, e: Pick<CalEvent, "calendar">) => colors.get(e.calendar.id) ?? "var(--series-other)"

/** an event's title; a calendar that keeps busy times only has none */
export function useTitle() {
  const t = useT()
  return (e: Pick<CalEvent, "title" | "busy_only">) => e.busy_only && !e.title ? t("calendar:grid.busy") : e.title?.trim() || t("calendar:grid.untitled")
}
/** the time of an event in a line: 10:30–11:30, or 全天 */
export function useWhen() {
  const t = useT()
  return (e: CalEvent, tz: string) => e.all_day
    ? (e.end && e.end !== e.start ? t("calendar:grid.allDayRange", { range: daysLabel(e.start, e.end) }) : t("calendar:grid.allDay"))
    : `${hmIn(e.start_ms, tz)}–${hmIn(e.end_ms, tz)}`
}
/** a block's tint: the calendar's colour mixed into the card, stronger in dark mode through the token itself */
export const tint = (color: string, pct = 16): CSSProperties => ({ backgroundColor: `color-mix(in srgb, ${color} ${pct}%, var(--card))` })

/* ───────── the timeline ───────── */
/** an hour's height in the timeline (px) */
export const HOUR_PX = 48
/** how many items an all-day cell shows before "还有 N 项" */
export const ALL_DAY_SHOWN = 3

function Block({ p, from, color, nav, tz, narrow }: { p: Placed; from: number; color: string; nav: Nav; tz: string; narrow: boolean }) {
  const t = useT(), title = useTitle()
  const e = p.e, h = ((p.bottom - p.top) / 60) * HOUR_PX
  const invite = awaitsReply(e), clash = !!e.conflicts_with?.length, no = declined(e)
  const style: CSSProperties = {
    top: ((p.top - from * 60) / 60) * HOUR_PX + 1, height: Math.max(h - 2, 14),
    left: `calc(${(p.col / p.cols) * 100}% + 2px)`, width: `calc(${100 / p.cols}% - 4px)`,
    ...(invite ? { borderColor: color } : { ...tint(color), borderLeftColor: color }),
  }
  return (
    <a href={nav.event(e.id)} style={style}
      className={cn("absolute z-[1] flex min-w-0 overflow-hidden rounded-md px-1.5 text-left", h < 30 ? "flex-row items-center" : "flex-col py-0.5", "text-foreground outline-none transition-shadow duration-120 hover:shadow-[var(--lift-hover)] focus-visible:ring-2 focus-visible:ring-ring",
        invite ? "border border-dashed bg-card/60" : "border-l-[3px]",
        clash && "shadow-[inset_-2px_0_0_var(--destructive)]", no && "opacity-55",
        nav.selected === e.id && "ring-2 ring-primary")}
      title={`${title(e)} · ${hmIn(e.start_ms, tz)}–${hmIn(e.end_ms, tz)}${invite ? " · " + t("calendar:grid.invite") : ""}${clash ? " · " + t("calendar:grid.conflict") : ""}`}
      data-cal-event={e.id} data-cal-invite={invite || undefined} data-cal-conflict={clash || undefined} data-cal-declined={no || undefined}>
      <span className={cn("truncate type-label font-medium", h < 30 && "leading-none", no && "line-through")}>{title(e)}</span>
      {h >= 34 && !narrow && <span className="truncate type-label text-muted-foreground tabular-nums">{p.cutStart ? "…" : hmIn(e.start_ms, tz)}–{p.cutEnd ? "…" : hmIn(e.end_ms, tz)}{invite ? " · " + t("calendar:grid.invite") : ""}</span>}
    </a>
  )
}

/** one of the all-day row's chips: an all-day event, a todo's deadline, a goal's target day */
function AllDayChip({ x, colors, nav }: { x: { e: CalEvent } | DayMark; colors: Colors; nav: Nav }) {
  const t = useT(), title = useTitle()
  if ("e" in x) {
    const c = colorFor(colors, x.e)
    return <a href={nav.event(x.e.id)} style={{ ...tint(c, 20), borderLeftColor: c }} className={cn("block truncate rounded-[5px] border-l-[3px] px-1.5 py-0.5 type-label text-foreground hover:brightness-95", awaitsReply(x.e) && "border border-dashed bg-transparent")} data-cal-allday={x.e.id}>{title(x.e)}</a>
  }
  if (x.kind === "goal") return <a href={"#/todos/" + x.todo.id} className="flex min-w-0 items-center gap-1 rounded-[5px] bg-primary/10 px-1.5 py-0.5 type-label text-primary hover:bg-primary/15" title={t("calendar:grid.goal", { text: x.todo.text })} data-cal-mark="goal">
    <FlagIcon className="size-3 shrink-0" aria-hidden /><span className="truncate">{x.todo.text}</span></a>
  return <a href={"#/todos/" + x.todo.id} className={cn("flex min-w-0 items-center gap-1 rounded-[5px] bg-muted px-1.5 py-0.5 type-label hover:bg-accent", x.overdue ? "text-destructive" : "text-foreground/85")} title={t(x.overdue ? "calendar:grid.overdue" : "calendar:grid.due", { text: x.todo.text })} data-cal-mark="todo">
    <SquareIcon className="size-3 shrink-0" aria-hidden /><span className="truncate">{x.todo.text}</span></a>
}

/** The timeline of `days` side by side: their heads, an all-day row with all-day events, todo deadlines and goals' target
 *  days, then the hours (08:00–20:00, widened to every event) with each day's events in columns where they overlap. */
export function TimeGrid({ days, events, todos, tz, now, nav, colors }: { days: string[]; events: CalEvent[]; todos: Todo[]; tz: string; now: number; nav: Nav; colors: Colors }) {
  const t = useT()
  const placed = days.map((d) => layoutDay(events, d, tz)), [h0, h1] = hoursOf(placed)
  const today = dayIn(now, tz), z = zoned(now, tz), nowMin = z.hour * 60 + z.minute
  const allDay = days.map((d) => [...events.filter((e) => e.all_day && e.status !== "cancelled" && onDay(e, d, tz)).map((e) => ({ e })), ...marksOn(todos, d, tz, now)])
  const most = Math.max(0, ...allDay.map((x) => x.length))
  const narrow = days.length >= 5
  const cols: CSSProperties = { gridTemplateColumns: `3rem repeat(${days.length}, minmax(0, 1fr))` }
  const [more, setMore] = useState(false)
  return (
    <div className="min-w-0 overflow-hidden rounded-xl border bg-card" data-cal-grid={days.length}>
      <div className="grid border-b" style={cols}>
        <span />
        {days.map((d) => (
          <a key={d} href={nav.day(d)} className="flex min-w-0 items-baseline gap-1.5 border-l px-2 py-2 hover:bg-accent/50" aria-current={d === today ? "date" : undefined} data-cal-day-head={d}>
            <span className={cn("type-footnote", d === today ? "text-primary" : "text-muted-foreground")}>{weekdayLabel(d)}</span>
            <span className={cn("type-headline tabular-nums", d === today ? "text-primary" : "text-foreground")}>{Number(d.slice(8))}</span>
          </a>
        ))}
      </div>
      {most > 0 && (
        <div className="grid border-b" style={cols} data-cal-allday-row>
          <span className="px-1.5 pt-1.5 text-right type-label text-tertiary">{t("calendar:grid.allDay")}</span>
          {allDay.map((xs, i) => (
            <div key={days[i]} className="flex min-w-0 flex-col gap-0.5 border-l p-1" data-cal-allday-cell={days[i]}>
              {(more ? xs : xs.slice(0, ALL_DAY_SHOWN)).map((x) => <AllDayChip key={"e" in x ? x.e.id : x.kind + x.todo.id} x={x} colors={colors} nav={nav} />)}
              {!more && xs.length > ALL_DAY_SHOWN && <button type="button" className="px-1.5 text-left type-label text-muted-foreground hover:text-foreground" onClick={() => setMore(true)} data-cal-allday-more>{t("calendar:grid.more", { count: xs.length - ALL_DAY_SHOWN })}</button>}
            </div>
          ))}
        </div>
      )}
      <div className="grid" style={cols}>
        <div className="relative" style={{ height: (h1 - h0) * HOUR_PX }}>
          {Array.from({ length: h1 - h0 }, (_, i) => <span key={i} className={cn("absolute right-1.5 type-label tabular-nums text-tertiary", i > 0 && "-translate-y-1/2")} style={{ top: i * HOUR_PX + (i ? 0 : 2) }}>{String(h0 + i).padStart(2, "0")}:00</span>)}
        </div>
        {days.map((d, i) => (
          <div key={d} className={cn("relative border-l", d === today && "bg-primary/[0.03]")} style={{ height: (h1 - h0) * HOUR_PX }} data-cal-col={d}>
            {Array.from({ length: h1 - h0 - 1 }, (_, k) => <span key={k} className="pointer-events-none absolute inset-x-0 border-t border-border/70" style={{ top: (k + 1) * HOUR_PX }} aria-hidden />)}
            {placed[i].map((p) => <Block key={p.e.id} p={p} from={h0} color={colorFor(colors, p.e)} nav={nav} tz={tz} narrow={narrow && p.cols > 1} />)}
            {d === today && nowMin >= h0 * 60 && nowMin <= h1 * 60 && (
              <div className="pointer-events-none absolute inset-x-0 z-[2] border-t-2 border-destructive" style={{ top: ((nowMin - h0 * 60) / 60) * HOUR_PX }} data-cal-now aria-label={t("calendar:grid.now")}>
                <span className="absolute -left-1 -top-[5px] size-2 rounded-full bg-destructive" />
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}

/** the marks under the grid: what dashed, a red edge and faded mean */
export function Legend() {
  const t = useT()
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 px-1 type-footnote text-muted-foreground" data-cal-legend>
      <span className="inline-flex items-center gap-1.5"><span className="h-3 w-4 rounded-[3px] border border-dashed border-muted-foreground" aria-hidden />{t("calendar:legend.invite")}</span>
      <span className="inline-flex items-center gap-1.5"><span className="h-3 w-4 rounded-[3px] bg-muted shadow-[inset_-2px_0_0_var(--destructive)]" aria-hidden />{t("calendar:legend.conflict")}</span>
      <span className="inline-flex items-center gap-1.5"><span className="h-3 w-4 rounded-[3px] bg-muted opacity-55" aria-hidden />{t("calendar:legend.declined")}</span>
      <span className="inline-flex items-center gap-1.5"><SquareIcon className="size-3" aria-hidden />{t("calendar:legend.due")}</span>
    </div>
  )
}

/* ───────── the agenda ───────── */
/** The agenda: each day that has something, its deadlines and goals, then its events in order with their time, calendar,
 *  place and what to look at (an invitation to answer, a conflict). */
export function AgendaList({ days, events, todos, tz, now, nav, colors }: { days: string[]; events: CalEvent[]; todos: Todo[]; tz: string; now: number; nav: Nav; colors: Colors }) {
  const t = useT(), title = useTitle(), when = useWhen()
  const today = dayIn(now, tz)
  const rows = days.map((d) => ({ d, xs: events.filter((e) => e.status !== "cancelled" && onDay(e, d, tz)).sort((a, b) => Number(b.all_day) - Number(a.all_day) || a.start_ms - b.start_ms), marks: marksOn(todos, d, tz, now) }))
    .filter((r) => r.xs.length || r.marks.length)
  if (!rows.length) return <div className="rounded-xl border border-dashed px-4 py-10 text-center type-callout text-muted-foreground" data-cal-agenda-empty>{t("calendar:agenda.none", { count: days.length })}</div>
  return (
    <div className="flex flex-col gap-6" data-cal-agenda>
      {rows.map(({ d, xs, marks }) => (
        <section key={d} className="min-w-0" data-cal-agenda-day={d}>
          <a href={nav.day(d)} className={cn("mb-1.5 flex items-baseline gap-2 px-1 type-label hover:text-foreground", d === today ? "text-primary" : "text-tertiary")}>
            {dayLong(d)}{d === today ? <span>{t("calendar:agenda.today")}</span> : d === plusDays(today, 1) ? <span>{t("calendar:agenda.tomorrow")}</span> : null}
          </a>
          {marks.length > 0 && <div className="mb-1.5 flex flex-wrap gap-1.5 px-1">{marks.map((m) => <span key={m.kind + m.todo.id} className="max-w-full"><AllDayChip x={m} colors={colors} nav={nav} /></span>)}</div>}
          <div className="flex flex-col">
            {xs.map((e) => {
              const c = colorFor(colors, e), invite = awaitsReply(e), clash = !!e.conflicts_with?.length, past = !e.all_day && e.end_ms <= now
              return (
                <a key={e.id} href={nav.event(e.id)} className={cn("flex min-h-14 items-stretch gap-3 rounded-lg px-1 py-2 hover:bg-accent", nav.selected === e.id && "bg-accent", (past || declined(e)) && "opacity-60")} data-cal-agenda-event={e.id}>
                  <span className="flex w-12 shrink-0 flex-col pt-0.5 type-footnote tabular-nums text-muted-foreground">
                    {e.all_day ? <span>{t("calendar:grid.allDay")}</span> : <><span className="text-foreground">{onDay(e, d, tz) && dayIn(e.start_ms, tz) === d ? hmIn(e.start_ms, tz) : "…"}</span><span>{dayIn(e.end_ms - 1, tz) === d ? hmIn(e.end_ms, tz) : "…"}</span></>}
                  </span>
                  <span className={cn("w-[3px] shrink-0 rounded-full", invite && "w-0 border-l-[3px] border-dotted")} style={invite ? { borderColor: c } : { backgroundColor: c }} aria-hidden />
                  <span className="min-w-0 flex-1">
                    <span className={cn("block truncate type-body", declined(e) && "line-through")}>{title(e)}</span>
                    <span className="flex min-w-0 flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground">
                      <span className="truncate">{e.calendar.name}</span>
                      {e.location && <><span aria-hidden>·</span><span className="max-w-48 truncate">{e.location}</span></>}
                      {e.all_day && e.end && e.end !== e.start && <><span aria-hidden>·</span><span>{when(e, tz)}</span></>}
                      {invite && <><span aria-hidden>·</span><span className="text-attention">{t("calendar:grid.invite")}</span></>}
                      {clash && <><span aria-hidden>·</span><span className="text-destructive">{t("calendar:grid.conflict")}</span></>}
                      {declined(e) && <><span aria-hidden>·</span><span>{t("calendar:grid.declined")}</span></>}
                    </span>
                  </span>
                </a>
              )
            })}
          </div>
        </section>
      ))}
    </div>
  )
}

/** a phone's week strip over the agenda: the seven days of the week, today circled, the first day shown marked */
export function WeekStrip({ days, today, start, nav }: { days: string[]; today: string; start: string; nav: Nav }) {
  return (
    <div className="grid grid-cols-7 gap-1" data-cal-strip>
      {days.map((d) => (
        <a key={d} href={nav.day(d)} className="flex flex-col items-center gap-1 rounded-lg py-1 hover:bg-accent" aria-current={d === start ? "date" : undefined} data-cal-strip-day={d}>
          <span className="type-label text-tertiary">{weekdayNarrow(d)}</span>
          <span className={cn("flex size-8 items-center justify-center rounded-full type-callout tabular-nums",
            d === today ? "bg-primary font-semibold text-primary-foreground" : d === start ? "bg-muted font-semibold text-foreground" : "text-foreground")}>{Number(d.slice(8))}</span>
        </a>
      ))}
    </div>
  )
}

/* ───────── the week view's rail ───────── */
/** a month to jump around in: the days a view shows are marked, today circled; its own arrows change the month only */
export function MiniMonth({ day, today, shown, nav }: { day: string; today: string; shown: string[]; nav: Nav }) {
  const t = useT()
  const [anchor, setAnchor] = useState(day.slice(0, 8) + "01")
  const weeks = monthGrid(anchor), month = anchor.slice(0, 7), shownSet = new Set(shown)
  const flip = (n: number) => { const [y, m] = anchor.split("-").map(Number), k = y * 12 + m - 1 + n; setAnchor(`${Math.floor(k / 12)}-${String((k % 12) + 1).padStart(2, "0")}-01`) }
  return (
    <div className="px-1" data-cal-month={month}>
      <div className="mb-1 flex items-center gap-1">
        <span className="flex-1 type-callout font-medium">{monthLabel(anchor)}</span>
        <button type="button" className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => flip(-1)} aria-label={t("calendar:rail.prevMonth")} data-cal-month-prev><ChevronLeftIcon className="size-4" /></button>
        <button type="button" className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => flip(1)} aria-label={t("calendar:rail.nextMonth")} data-cal-month-next><ChevronRightIcon className="size-4" /></button>
      </div>
      <div className="grid grid-cols-7 text-center">
        {weeks[0].map((d) => <span key={d} className="py-1 type-label text-tertiary">{weekdayNarrow(d)}</span>)}
        {weeks.flat().map((d) => (
          <a key={d} href={nav.day(d)} aria-label={dayLong(d)} aria-current={d === today ? "date" : undefined}
            className={cn("mx-auto my-0.5 flex size-7 items-center justify-center rounded-md type-footnote tabular-nums hover:bg-accent",
              d.slice(0, 7) !== month ? "text-tertiary/70" : "text-foreground/85", shownSet.has(d) && "bg-muted", d === today && "bg-primary font-semibold text-primary-foreground hover:bg-primary")}
            data-cal-month-day={d}>{Number(d.slice(8))}</a>
        ))}
      </div>
    </div>
  )
}

/** how fresh one calendar is, in a few words */
function fresh(c: CalSource["calendars"][number], t: ReturnType<typeof useT>, now: number) {
  if (!c.enabled) return { text: t("calendar:rail.off"), bad: false }
  if (c.sync_state === "error") return { text: t("calendar:rail.failed"), bad: true }
  if (!c.synced_at) return { text: t("calendar:rail.notYet"), bad: false }
  return { text: ago(c.synced_at, { now }), bad: false }
}
/** The sources by person: your calendars (each can be hidden from the page; the 接入 page turns one off), then each
 *  contact's, which give free/busy for finding time and are not drawn, then the ones nobody was named for yet (not
 *  drawn either until 接入 says whose they are). Every calendar says how fresh it is. */
export function SourcesRail({ sources, colors, hidden, onToggle, now }: { sources: CalSource[]; colors: Colors; hidden: Set<string>; onToggle: (id: string) => void; now: number }) {
  const t = useT()
  const all = sources.flatMap((s) => s.calendars.filter((c) => !c.gone).map((c) => ({ c, s })))
  const mine = all.filter((x) => x.c.mine), nobody = all.filter((x) => !x.c.mine && !x.c.party_id)
  const others = new Map<string, typeof all>()
  for (const x of all) if (!x.c.mine && x.c.party_id) others.set(x.c.party_name ?? x.c.party_id, [...(others.get(x.c.party_name ?? x.c.party_id) ?? []), x])
  const line = ({ c, s }: (typeof all)[number], toggle: boolean) => {
    const f = fresh(c, t, now), color = colors.get(c.id) ?? "var(--series-other)"
    return (
      <li key={c.id} className="flex min-h-8 items-center gap-2 px-1" data-cal-source={c.id}>
        {toggle ? <span className="flex shrink-0" style={{ "--aui-primary": color } as CSSProperties}><Checkbox checked={!hidden.has(c.id) && c.enabled} disabled={!c.enabled} onCheckedChange={() => onToggle(c.id)} aria-label={t("calendar:rail.show", { name: c.name })} data-cal-source-toggle={c.id} /></span>
          : <span className="size-2.5 shrink-0 rounded-full" style={{ backgroundColor: color }} aria-hidden />}
        <a href={"#/integrations/" + s.adapter} className="min-w-0 flex-1 truncate type-footnote text-foreground/85 hover:text-foreground" title={c.name}>{c.name}</a>
        <span className={cn("shrink-0 type-label", f.bad ? "text-destructive" : "text-tertiary")} title={c.sync_error ?? undefined} data-cal-source-state={c.sync_state}>{f.text}</span>
      </li>
    )
  }
  return (
    <div className="flex flex-col gap-4" data-cal-sources>
      <section>
        <div className="mb-1 px-1 type-label text-tertiary">{t("calendar:rail.mine")}</div>
        {mine.length ? <ul>{mine.map((x) => line(x, true))}</ul> : <p className="px-1 type-footnote text-muted-foreground">{t("calendar:rail.noneMine")}</p>}
      </section>
      {[...others].map(([name, xs]) => (
        <section key={name}>
          <div className="mb-0.5 px-1 type-label text-tertiary">{t("calendar:rail.others", { name })}</div>
          <p className="mb-1 px-1 type-label text-muted-foreground">{t("calendar:rail.othersNote")}</p>
          <ul>{xs.map((x) => line(x, false))}</ul>
        </section>
      ))}
      {nobody.length > 0 && (
        <section data-cal-sources-nobody>
          <div className="mb-0.5 px-1 type-label text-tertiary">{t("calendar:rail.nobody")}</div>
          <p className="mb-1 px-1 type-label text-muted-foreground">{t("calendar:rail.nobodyNote")}</p>
          <ul>{nobody.map((x) => line(x, false))}</ul>
        </section>
      )}
      <a href="#/integrations" className="inline-flex items-center gap-1 px-1 type-footnote text-muted-foreground hover:text-foreground" data-cal-manage>{t("calendar:rail.manage")}</a>
    </div>
  )
}

