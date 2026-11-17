/* 日历: your schedule from the calendars you connected under 接入 (the Hub's cache, read only), with your todos'
 * deadlines and your goals' target days. Times are in your zone as the Hub knows it.
 *
 *   #/calendar?view=day|week|agenda&d=YYYY-MM-DD   a view around a day (none: today). A phone has 议程, 日 and 3 日
 *   #/calendar/e/<id>?view=…&d=…                   an event's details: beside the view, or its own page on a phone
 *
 * Day: the timeline, and beside it the day's summary, the next meeting and the todos due. Week: a month and the
 * sources by person on the left, seven days on the right. Agenda: a list by day, a phone's default. Without a source
 * the page is the connect prompt. Opening the page asks each source for what changed (the Hub answers from its cache
 * when it synced within the last minute); every sync shows up through the `calendar` stream event. */
import { useEffect, useMemo, useState } from "react"
import { ChevronLeftIcon, ChevronRightIcon, XIcon } from "lucide-react"
import { calendarApi } from "@/api/calendar"
import { Button } from "@/components/ui/button"
import { useIsMobile } from "@/hooks/use-mobile"
import { useT } from "@/i18n"
import { ago } from "@/i18n/format"
import { useStore } from "@/store"
import { useMinute, useTodos } from "@/todos/data"
import { dueDay, isOverdue, type Todo } from "@/todos/model"
import { Empty, go, Loading, Page, Segmented } from "@/ui"
import { knownTz, loadSources, useAgenda, useSources } from "./data"
import { EventBody } from "./event"
import { daysLabel } from "./format"
import { browserTz, colorOf, dayIn, sameClock, daysOf, isDay, nextMeeting, onDay, step, summarize, todoDay, type CalEvent, type View } from "./model"
import { AgendaList, Legend, MiniMonth, SourcesRail, TimeGrid, WeekStrip, type Colors, type Nav } from "./parts"
import { DayCard, DueList, NextCard } from "./side"
import { Unconnected } from "./unconnected"

const VIEWS: View[] = ["day", "week", "three", "agenda"]
const DESK: View[] = ["day", "week", "agenda"], PHONE: View[] = ["agenda", "day", "three"]
/** the page asks the sources for changes at most this often while it is open (the Hub's own floor is a minute) */
const SYNC_ON_OPEN_MS = 5 * 60_000
let lastSyncAsk = 0
/** calendars hidden from the page (not from syncing: that is the 接入 page's switch), kept in this browser */
const HIDDEN_KEY = "steve-cal-hidden"
const readHidden = () => { try { return new Set<string>(JSON.parse(localStorage.getItem(HIDDEN_KEY) ?? "[]")) } catch { return new Set<string>() } }

/** at least `px` wide: the event panel sits beside the view; narrower, an event is its own page */
function useWide(px = 1024) {
  const q = `(min-width: ${px}px)`
  const [on, setOn] = useState(() => typeof matchMedia === "function" && matchMedia(q).matches)
  useEffect(() => { const m = matchMedia(q), f = () => setOn(m.matches); m.addEventListener("change", f); f(); return () => m.removeEventListener("change", f) }, [q])
  return on
}

export function CalendarView({ query, event }: { query?: string; event?: string }) {
  useStore()
  const t = useT(), now = useMinute(), mobile = useIsMobile(), wide = useWide()
  const src = useSources(), todosState = useTodos()
  const q = new URLSearchParams(query ?? "")
  const tz = knownTz(), today = dayIn(now, tz)
  const d = isDay(q.get("d")) ? q.get("d")! : today
  const asked = q.get("view") as View | null
  let view: View = asked && VIEWS.includes(asked) ? asked : mobile ? "agenda" : "day"
  if (mobile && view === "week") view = "three"
  if (!mobile && view === "three") view = "week"
  const sources = src.sources, connected = !!sources && sources.length > 0
  const ag = useAgenda(view, d, connected)
  const zone = ag.agenda?.range.tz ?? tz

  /* ask each source for what changed when the page opens */
  useEffect(() => {
    if (!connected || Date.now() - lastSyncAsk < SYNC_ON_OPEN_MS) return
    lastSyncAsk = Date.now()
    for (const s of sources!) void calendarApi.sync(s.connection_id).catch(() => {})
  }, [connected]) // eslint-disable-line react-hooks/exhaustive-deps

  const [hidden, setHidden] = useState(readHidden)
  const toggle = (id: string) => setHidden((h) => { const n = new Set(h); if (!n.delete(id)) n.add(id); try { localStorage.setItem(HIDDEN_KEY, JSON.stringify([...n])) } catch { /* private mode */ } return n })
  const colors: Colors = useMemo(() => { const m = new Map<string, string>(); let n = 0; for (const s of sources ?? []) for (const c of s.calendars) m.set(c.id, colorOf(c.color, n++)); return m }, [sources])
  const events: CalEvent[] = (ag.agenda?.events ?? []).filter((e) => !hidden.has(e.calendar.id))
  const todos: Todo[] = todosState.todos ?? []

  const href = (v: View, day: string, ev?: string) => `#/calendar${ev ? "/e/" + encodeURIComponent(ev) : ""}?view=${v}${day !== today ? "&d=" + day : ""}`
  const nav: Nav = { day: (x) => href(view, x), event: (id) => href(view, d, id), selected: event }
  const gridNav: Nav = { ...nav, day: (x) => href("day", x) }
  const days = daysOf(view, d)

  /* the page's head: today, back and forth, which days, and the views */
  const opts = (mobile ? PHONE : DESK).map((v) => [v, t(`calendar:view.${v}`)] as const)
  const toolbar = connected && (
    <div className="flex flex-wrap items-center gap-2" data-cal-toolbar>
      <Button size="sm" variant="outline" onClick={() => go(href(view, today))} disabled={d === today} data-cal-today>{t("calendar:nav.today")}</Button>
      <Button size="icon-sm" variant="ghost" aria-label={t("calendar:nav.prev")} onClick={() => go(href(view, step(view, d, -1)))} data-cal-step="prev"><ChevronLeftIcon /></Button>
      <Button size="icon-sm" variant="ghost" aria-label={t("calendar:nav.next")} onClick={() => go(href(view, step(view, d, 1)))} data-cal-step="next"><ChevronRightIcon /></Button>
      <span className="min-w-0 type-callout font-medium tabular-nums" data-cal-label>{daysLabel(days[0], days[days.length - 1])}</span>
      <Segmented value={view} onChange={(v) => go(href(v, d))} label={t("calendar:view.label")} options={opts} size="sm" />
    </div>
  )
  const back: [string, string] = [href(view, d), t("calendar:title")]

  /* how fresh the copy is: the oldest of your calendars, and how many failed */
  const own = (sources ?? []).flatMap((s) => s.calendars).filter((c) => c.enabled && !c.gone && c.mine)
  const synced = own.map((c) => c.synced_at).filter((x): x is number => x != null)
  const failed = own.filter((c) => c.sync_state === "error").length
  const sub = connected ? (
    <span className="flex flex-wrap gap-x-2" data-cal-fresh>
      <span>{synced.length ? t("calendar:fresh.asOf", { ago: ago(Math.min(...synced), { now }) }) : t("calendar:fresh.notYet")}</span>
      {failed > 0 && <span className="text-destructive" data-cal-fresh-failed>{t("calendar:fresh.failed", { count: failed })}</span>}
      {!sameClock(now, zone, browserTz()) && <span data-cal-zone>{t("calendar:fresh.zone", { tz: zone })}</span>}
    </span>
  ) : t("calendar:sub")

  /* an event on a phone (or a narrow window): its own page */
  if (event && !wide) return (
    <Page title={t("calendar:detail.title")} back={back} small>
      <EventBody id={event} tz={zone} now={now} colors={colors} />
    </Page>
  )

  let body
  if (!sources) body = src.error && !src.unwired
    ? <Empty title={t("calendar:failed")} sub={src.error} action={<Button variant="outline" onClick={() => void loadSources()}>{t("common:action.retry")}</Button>} data-cal-error />
    : src.unwired ? <Unconnected unwired /> : <Loading />
  else if (!connected) body = <Unconnected unwired={false} />
  else if (!ag.agenda) body = ag.error
    ? <Empty title={t("calendar:failed")} sub={ag.error} action={<Button variant="outline" onClick={ag.reload}>{t("common:action.retry")}</Button>} data-cal-error />
    : <Loading />
  else {
    /* what sits beside a view: the open event, else the day's summary, the next meeting, the todos due */
    const sideDay = view === "agenda" ? (days.includes(today) ? today : d) : d
    const dueOn = todos.filter((x) => x.kind === "todo" && x.state === "open" && (todoDay(x, zone) === sideDay || (sideDay === today && dueDay(x) != null && isOverdue(x, now))))
    const sum = summarize(events, todos, sideDay, zone, now)
    const next = sideDay === today ? nextMeeting(events.filter((e) => onDay(e, today, zone)), now) : null
    const side = (
      <div className="flex min-w-0 flex-col gap-6" data-cal-side>
        <DayCard s={sum} day={sideDay} today={today} tz={zone} nav={nav} events={events} />
        {next && <NextCard e={next} now={now} tz={zone} nav={nav} colors={colors} />}
        <DueList day={sideDay} today={today} todos={dueOn} now={now} />
      </div>
    )
    const panel = event && (
      <aside className="min-w-0 self-start rounded-xl border bg-card p-4 lg:sticky lg:top-2" data-cal-panel>
        <div className="-mt-1 mb-2 flex justify-end"><Button size="icon-sm" variant="ghost" className="text-muted-foreground" aria-label={t("calendar:detail.close")} onClick={() => go(href(view, d))} data-cal-panel-close><XIcon /></Button></div>
        <EventBody id={event} tz={zone} now={now} colors={colors} />
      </aside>
    )
    const grid = (ds: string[]) => <div className="flex min-w-0 flex-col gap-3"><TimeGrid days={ds} events={events} todos={todos} tz={zone} now={now} nav={view === "day" ? nav : gridNav} colors={colors} /><Legend /></div>
    const agenda = <AgendaList days={days} events={events} todos={todos} tz={zone} now={now} nav={nav} colors={colors} />
    const strip = <WeekStrip days={daysOf("week", d)} today={today} start={d} nav={nav} />
    if (mobile) body = (
      <div className="flex flex-col gap-5" data-cal-view={view}>
        {view !== "three" && strip}
        {view === "agenda" ? <>{days.includes(today) && <DayCard s={sum} day={today} today={today} tz={zone} nav={nav} events={events} />}{next && <NextCard e={next} now={now} tz={zone} nav={nav} colors={colors} />}{agenda}</>
          : view === "day" ? <>{grid(days)}<DueList day={d} today={today} todos={dueOn} now={now} /></> : grid(days)}
      </div>
    )
    else if (view === "week") body = (
      <div className={event ? "grid gap-6 lg:grid-cols-[minmax(0,1fr)_340px]" : "grid gap-6 lg:grid-cols-[208px_minmax(0,1fr)]"} data-cal-view="week">
        {!event && <aside className="flex min-w-0 flex-col gap-6" data-cal-rail><MiniMonth key={d} day={d} today={today} shown={days} nav={nav} /><SourcesRail sources={sources} colors={colors} hidden={hidden} onToggle={toggle} now={now} /></aside>}
        {grid(days)}
        {panel}
      </div>
    )
    else body = (
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_340px]" data-cal-view={view}>
        {view === "day" ? grid(days) : agenda}
        {panel || side}
      </div>
    )
  }
  return (
    <Page title={t("calendar:title")} sub={sub} max={1280} toolbar={toolbar || undefined}>
      <div data-calendar={connected ? "connected" : undefined}>{body}</div>
    </Page>
  )
}
