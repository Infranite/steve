/* What sits beside a day: its summary, the next meeting, and the todos due. The summary is collected by fixed rules
 * (no model): the meetings and where two overlap, invitations not answered yet, todos due and overdue, the goals whose
 * target falls this week, the cards waiting in 待处理, and free time from the Hub's find-time. It is recomputed live
 * from what the page shows; the pushed morning briefing is a separate thing. */
import { ArrowRightIcon, ClockIcon, StarIcon, SunIcon, VideoIcon } from "lucide-react"
import { cn } from "cn"
import { Checkbox } from "@/components/ui/checkbox"
import type { ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { useT } from "@/i18n"
import { pendingCards } from "@/store"
import { Card, Section } from "@/ui"
import { useTodos } from "@/todos/data"
import { isOverdue, type Todo } from "@/todos/model"
import { complete, useDueText } from "@/todos/parts"
import { useFreeTime } from "./data"
import { dayLabel, untilText } from "./format"
import { FREE_MIN, hmIn, safeUrl, type CalEvent, type DaySummary } from "./model"
import { useTitle, type Colors, type Nav } from "./parts"

function Line({ label, children, href, k, open }: { label: string; children: ReactNode; href?: string; k: string; open: string }) {
  return (
    <div className="flex min-h-10 items-start gap-3 border-t py-2 first:border-t-0" data-cal-sum={k}>
      <span className="w-10 shrink-0 pt-px type-footnote text-muted-foreground">{label}</span>
      <div className="min-w-0 flex-1 type-callout">{children}</div>
      {href && <a href={href} className="shrink-0 rounded-md px-1.5 py-0.5 type-footnote text-muted-foreground hover:bg-accent hover:text-foreground" data-cal-sum-open={k}>{open}</a>}
    </div>
  )
}

/** The day's summary: one sentence by template, then a line each for what is there. Quiet days say so. */
export function DayCard({ s, day, today, tz, nav, events }: { s: DaySummary; day: string; today: string; tz: string; nav: Nav; events: CalEvent[] }) {
  const t = useT(), title = useTitle()
  const isToday = day === today, waiting = isToday ? pendingCards().length : 0
  const free = useFreeTime(day >= today ? day : null, FREE_MIN, events.length + ":" + events.map((e) => e.version).join())
  const byId = new Map(events.map((e) => [e.id, e]))
  const clash = s.conflicts[0], other = clash ? byId.get(clash.conflicts_with![0]) : undefined
  const parts = [
    s.meetings ? t("calendar:sum.meetings", { count: s.meetings }) : null,
    s.conflicts.length ? t("calendar:sum.conflicts", { count: Math.max(1, Math.floor(s.conflicts.length / 2)) }) : null,
    s.invites.length ? t("calendar:sum.invites", { count: s.invites.length }) : null,
    s.dueToday.length ? t("calendar:sum.due", { count: s.dueToday.length }) : null,
    isToday && s.overdue.length ? t("calendar:sum.overdue", { count: s.overdue.length }) : null,
  ].filter(Boolean)
  const head = s.quiet && !waiting ? t(isToday ? "calendar:sum.quietToday" : "calendar:sum.quiet") : t("calendar:sum.sentence", { list: parts.join(t("calendar:sum.sep")) || t("calendar:sum.noMeetings") })
  const open = t("calendar:sum.open")
  const g = s.goals[0]
  return (
    <Card className="flex flex-col gap-2" data-cal-summary={day}>
      <div className="flex items-center gap-2 type-footnote text-muted-foreground">
        <SunIcon className="size-3.5" aria-hidden /><span className="flex-1">{isToday ? t("calendar:sum.today") : dayLabel(day)}</span><span className="text-tertiary">{t("calendar:sum.rules")}</span>
      </div>
      <p className="type-headline" data-cal-sum-head>{head}</p>
      <div className="flex flex-col">
        {s.meetings > 0 && <Line k="meetings" label={t("calendar:sum.lMeetings")} open={open} href={clash ? nav.event(clash.id) : undefined}>
          {t("calendar:sum.vMeetings", { count: s.meetings })}
          {clash && other && <span className="text-destructive">{t("calendar:sum.sep")}{t("calendar:sum.vClash", { a: hmIn(clash.start_ms, tz), b: hmIn(other.start_ms, tz) })}</span>}
        </Line>}
        {s.invites.length > 0 && <Line k="invites" label={t("calendar:sum.lInvites")} open={open} href={nav.event(s.invites[0].id)}>
          {t("calendar:sum.vInvite", { title: title(s.invites[0]) })}{s.invites.length > 1 && <span className="text-muted-foreground">{t("calendar:sum.sep")}{t("calendar:sum.vMore", { count: s.invites.length - 1 })}</span>}
        </Line>}
        {(s.dueToday.length > 0 || (isToday && s.overdue.length > 0)) && <Line k="todos" label={t("calendar:sum.lTodos")} open={open} href="#/todos">
          {t("calendar:sum.vDue", { count: s.dueToday.length })}
          {isToday && s.overdue.length > 0 && <span className="text-destructive">{t("calendar:sum.sep")}{t("calendar:sum.vOverdue", { count: s.overdue.length })}</span>}
        </Line>}
        {g && <Line k="goals" label={t("calendar:sum.lGoals")} open={open} href={"#/todos/" + g.id}>
          {t("calendar:sum.vGoal", { text: g.text, progress: g.progress?.pct != null ? `${g.progress.pct}%` : t("calendar:sum.unsplit"), day: dayLabel(g.target_date!) })}
          {s.goals.length > 1 && <span className="text-muted-foreground">{t("calendar:sum.sep")}{t("calendar:sum.vMore", { count: s.goals.length - 1 })}</span>}
        </Line>}
        {waiting > 0 && <Line k="waiting" label={t("calendar:sum.lWaiting")} open={open} href="#/inbox">{t("calendar:sum.vWaiting", { count: waiting })}</Line>}
        {free && (free.slots.length > 0 || s.meetings > 0) && <Line k="free" label={t("calendar:sum.lFree")} open={open}>
          {free.slots.length ? free.slots.map((x) => `${hmIn(x.start_ms, tz)}–${hmIn(x.end_ms, tz)}`).join(t("calendar:sum.sep")) : <span className="text-muted-foreground">{t("calendar:sum.vNoFree", { min: FREE_MIN })}</span>}
        </Line>}
      </div>
    </Card>
  )
}

/** the meeting on now or next: when, what, where, and a button to join it */
export function NextCard({ e, now, tz, nav, colors }: { e: CalEvent; now: number; tz: string; nav: Nav; colors: Colors }) {
  const t = useT(), title = useTitle()
  const until = untilText(e.start_ms, e.end_ms, now), color = colors.get(e.calendar.id) ?? "var(--series-other)"
  return (
    <Section label={t("calendar:next.head")}>
      <Card className="relative flex flex-col gap-1.5" data-cal-next={e.id}>
        <a href={nav.event(e.id)} className="absolute inset-0 rounded-[inherit]" aria-label={title(e)} />
        <div className="flex items-center gap-1.5 type-footnote text-primary"><ClockIcon className="size-3.5" aria-hidden />{until ? t("calendar:next.when", { until, time: hmIn(e.start_ms, tz) }) : hmIn(e.start_ms, tz)}</div>
        <div className="flex min-w-0 items-center gap-2"><span className="size-2 shrink-0 rounded-full" style={{ backgroundColor: color }} aria-hidden /><span className="truncate type-headline">{title(e)}</span></div>
        <div className="flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground">
          <span>{hmIn(e.start_ms, tz)}–{hmIn(e.end_ms, tz)}</span><span aria-hidden>·</span><span className="truncate">{e.calendar.name}</span>
          {e.location && <><span aria-hidden>·</span><span className="max-w-48 truncate">{e.location}</span></>}
          {e.attendees ? <><span aria-hidden>·</span><span>{t("calendar:next.people", { count: e.attendees })}</span></> : null}
        </div>
        {safeUrl(e.meeting_url) && <div className="relative z-[1] pt-1"><Button size="sm" nativeButton={false} render={<a href={safeUrl(e.meeting_url)!} target="_blank" rel="noopener noreferrer" />} data-cal-join><VideoIcon />{t("calendar:detail.join")}</Button></div>}
      </Card>
    </Section>
  )
}

/** one todo in the side column: a box to tick, its words (they open it), its deadline; the todo page has the rest */
function DueRow({ x, now }: { x: Todo; now: number }) {
  const t = useT(), due = useDueText()
  const late = isOverdue(x, now)
  return (
    <div role="listitem" className="relative flex min-h-10 items-start gap-2.5 rounded-lg px-3 py-2 hover:bg-accent" data-todo={x.id}>
      <span className="relative z-[1] pt-0.5"><Checkbox checked={false} onCheckedChange={(v) => void complete(x, v)} aria-label={t("todos:row.done", { text: x.text })} data-todo-check /></span>
      <a href={"#/todos/" + x.id} className="min-w-0 flex-1 type-callout outline-none after:absolute after:inset-0 after:rounded-[inherit]" data-todo-open>
        {x.star && <StarIcon className="-mt-0.5 mr-1 inline size-3.5 fill-current text-attention" aria-label={t("todos:row.starred")} />}{x.text}
      </a>
      <span className={cn("shrink-0 pt-px type-footnote tabular-nums", late ? "text-destructive" : "text-muted-foreground")} data-todo-due>{due(x, now)}</span>
    </div>
  )
}
/** the todos due on a day (today: and the overdue ones) */
export function DueList({ day, today, todos, now }: { day: string; today: string; todos: Todo[]; now: number }) {
  const t = useT(), s = useTodos()
  if (!s.todos) return null
  return (
    <Section label={day === today ? t("calendar:due.today") : t("calendar:due.day", { day: dayLabel(day) })} right={<a href="#/todos" className="inline-flex items-center gap-0.5 type-footnote text-muted-foreground hover:text-foreground">{t("calendar:due.all")}<ArrowRightIcon className="size-3" /></a>} data-cal-due={day}>
      {todos.length ? <div role="list" className="-mx-3 flex flex-col">{todos.map((x) => <DueRow key={x.id} x={x} now={now} />)}</div>
        : <p className="px-1 type-footnote text-muted-foreground">{t("calendar:due.none")}</p>}
    </Section>
  )
}
