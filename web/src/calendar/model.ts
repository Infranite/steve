/* 日历's rules as plain functions without React, so node --test can check them: the shapes the Hub's calendar routes
 * answer with (steve-calendar `query.rs`), days and times in the reader's time zone, the range each view reads, how
 * events stack in a day, the month grid, and the day's summary, all by fixed rules.
 *
 * Time zone: the Hub resolves the reader's zone (their notification setting, else the workspace's) and answers with it
 * (`range.tz`); every day and clock time of the page is in that zone, not the browser's, so the page, the agents and
 * the reminders say the same times. Days are "YYYY-MM-DD" strings; their arithmetic is on the calendar, not on
 * milliseconds, so a daylight-saving change never moves a day. */

import { isOverdue, type Todo } from "../todos/model.ts"

/* ───────── what the Hub answers ───────── */
export type CalRange = { from: string; to: string; tz: string; from_ms: number; to_ms: number }
export type Organizer = { name: string | null; email: string | null; party_id: string | null; me: boolean }
/** one meeting as the agenda shows it: the occurrence in its main calendar, `also_in` the other calendars that have it */
export type CalEvent = {
  id: string; meeting_id: string | null; calendar: { id: string; name: string }
  title: string | null; all_day: boolean; start_ms: number; end_ms: number
  /** local "YYYY-MM-DD HH:MM", or for an all-day event its first and last day (inclusive) */
  start: string; end: string | null
  status: "confirmed" | "tentative" | "cancelled" | string; busy_only: boolean; version: number
  location?: string; meeting_url?: string; excerpt?: string
  my_rsvp?: "accepted" | "declined" | "tentative" | "needs_action" | string
  organizer?: Organizer; also_in?: string[]; attendees?: number; conflicts_with?: string[]
}
export type CalStatus = { id: string; name: string; state: "new" | "ok" | "error" | string; as_of: string | null; error: string | null }
export type Agenda = { range: CalRange; events: CalEvent[]; calendars: CalStatus[] }
export type Attendee = { party_id: string | null; email: string | null; name: string | null; ext_ref: string | null; rsvp: string | null; organizer: boolean; optional: boolean; is_self: boolean }
/** one event read on its own: everyone in it, and how fresh its calendar is */
export type EventDetail = Omit<CalEvent, "attendees"> & { attendees: Attendee[]; deleted?: boolean; as_of?: string | null }
export type Slot = { start: string; end: string; start_ms: number; end_ms: number; why: string[] }
export type FreeTime = { range: CalRange; duration_min: number; slots: Slot[] }

/* ───────── days and times in a zone ───────── */
const fmts = new Map<string, Intl.DateTimeFormat>()
function fmt(tz: string) {
  let f = fmts.get(tz)
  if (!f) {
    try { f = new Intl.DateTimeFormat("en-US", { timeZone: tz, year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23" }) }
    catch { f = new Intl.DateTimeFormat("en-US", { timeZone: "UTC", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23" }) }
    fmts.set(tz, f)
  }
  return f
}
/** a moment's local date and clock time in `tz` */
export function zoned(ms: number, tz: string) {
  const p: Record<string, string> = {}
  for (const x of fmt(tz).formatToParts(new Date(ms))) p[x.type] = x.value
  const hour = Number(p.hour) % 24
  return { day: `${p.year}-${p.month}-${p.day}`, hour, minute: Number(p.minute) }
}
const pad = (n: number) => String(n).padStart(2, "0")
/** the local day of a moment in `tz` */
export const dayIn = (ms: number, tz: string) => zoned(ms, tz).day
/** the local clock time of a moment in `tz`, "HH:MM" */
export const hmIn = (ms: number, tz: string) => { const z = zoned(ms, tz); return `${pad(z.hour)}:${pad(z.minute)}` }
/** two zones show the same date and clock time at `ms` (Asia/Taipei and Asia/Shanghai do) */
export const sameClock = (ms: number, a: string, b: string) => { const x = zoned(ms, a), y = zoned(ms, b); return x.day === y.day && x.hour === y.hour && x.minute === y.minute }
/** the browser's own zone */
export const browserTz = () => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC" } catch { return "UTC" } }

const utc = (day: string) => { const [y, m, d] = day.split("-").map(Number); return Date.UTC(y, m - 1, d) }
const ymd = (ms: number) => { const d = new Date(ms); return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}` }
/** the day `n` days after `day` */
export const plusDays = (day: string, n: number) => ymd(utc(day) + n * 86_400_000)
/** days from `a` to `b` */
export const daysBetween = (a: string, b: string) => Math.round((utc(b) - utc(a)) / 86_400_000)
/** Monday = 1 … Sunday = 7 */
export const isoWeekday = (day: string) => ((new Date(utc(day)).getUTCDay() + 6) % 7) + 1
/** the Monday of the week `day` is in */
export const mondayOf = (day: string) => plusDays(day, 1 - isoWeekday(day))
/** a day as a Date for formatting its name: noon UTC, so any display zone keeps the same date */
export const dateForName = (day: string) => new Date(utc(day) + 12 * 3_600_000)
export const isDay = (s: string | null | undefined): s is string => !!s && /^\d{4}-\d{2}-\d{2}$/.test(s) && ymd(utc(s)) === s

/** the six weeks (Monday first) of the month `day` is in, so the grid never changes height */
export function monthGrid(day: string): string[][] {
  const first = day.slice(0, 8) + "01", start = mondayOf(first)
  return Array.from({ length: 6 }, (_, w) => Array.from({ length: 7 }, (_, d) => plusDays(start, w * 7 + d)))
}

/* ───────── views and what each reads ───────── */
export type View = "day" | "week" | "three" | "agenda"
/** how many days the agenda lists from the day it starts on */
export const AGENDA_DAYS = 14
/** the days a view shows around `day` */
export function daysOf(view: View, day: string): string[] {
  const n = view === "day" ? 1 : view === "three" ? 3 : view === "agenda" ? AGENDA_DAYS : 7
  const start = view === "week" ? mondayOf(day) : day
  return Array.from({ length: n }, (_, i) => plusDays(start, i))
}
/** the day the 上一页 / 下一页 buttons go to */
export const step = (view: View, day: string, dir: 1 | -1) => plusDays(view === "week" ? mondayOf(day) : day, dir * (view === "day" ? 1 : view === "three" ? 3 : 7))
/** the query of the events route for a view: whole local days, `to` a date meaning the end of that day */
export function rangeOf(view: View, day: string) { const ds = daysOf(view, day); return { from: ds[0], to: ds[ds.length - 1] } }

/** the days an event is on: an all-day one its dates, a timed one each day it touches */
export function eventDays(e: CalEvent, tz: string): [string, string] {
  if (e.all_day) return [e.start.slice(0, 10), (e.end ?? e.start).slice(0, 10)]
  const a = dayIn(e.start_ms, tz), b = dayIn(Math.max(e.start_ms, e.end_ms - 1), tz)
  return [a, b]
}
export const onDay = (e: CalEvent, day: string, tz: string) => { const [a, b] = eventDays(e, tz); return a <= day && day <= b }

/** an invitation you have not answered yet */
export const awaitsReply = (e: Pick<CalEvent, "my_rsvp" | "status">) => e.my_rsvp === "needs_action" && e.status !== "cancelled"
/** you said no: drawn faded, never busy, never the next meeting */
export const declined = (e: Pick<CalEvent, "my_rsvp">) => e.my_rsvp === "declined"
/** what the page calls a meeting: timed, not cancelled, not declined */
export const isMeeting = (e: CalEvent) => !e.all_day && e.status !== "cancelled" && !declined(e)

/* ───────── a day's timeline ───────── */
/** the hours a day's timeline always shows; an earlier or later event widens it to its whole hour */
export const DAY_FROM_HOUR = 8
export const DAY_TO_HOUR = 20
/** the shortest block drawn, in minutes, so a 5-minute event still has room for its title */
export const MIN_BLOCK_MIN = 22

export type Placed = { e: CalEvent; top: number; bottom: number; col: number; cols: number; cutStart: boolean; cutEnd: boolean }
/** where a timed event sits on `day`, in minutes after local midnight (end exclusive), clipped to the day */
export function minutesOn(e: CalEvent, day: string, tz: string): [number, number, boolean, boolean] {
  const s = zoned(e.start_ms, tz), last = zoned(Math.max(e.start_ms, e.end_ms - 1), tz)
  const cutStart = s.day < day, cutEnd = last.day > day
  const a = cutStart ? 0 : s.hour * 60 + s.minute
  const b = cutEnd ? 24 * 60 : e.end_ms <= e.start_ms ? a : last.hour * 60 + last.minute + 1
  return [a, Math.max(b, a), cutStart, cutEnd]
}
/** the timed events of `day` side by side: events that overlap share the width in columns, each takes the first free
 *  column, and a group of overlapping events keeps one column count */
export function layoutDay(events: CalEvent[], day: string, tz: string): Placed[] {
  const xs = events.filter((e) => !e.all_day && e.status !== "cancelled" && onDay(e, day, tz)).map((e) => {
    const [top, bottom, cutStart, cutEnd] = minutesOn(e, day, tz)
    return { e, top, bottom: Math.max(bottom, top + MIN_BLOCK_MIN), col: 0, cols: 1, cutStart, cutEnd }
  }).sort((x, y) => x.top - y.top || y.bottom - x.bottom || x.e.id.localeCompare(y.e.id))
  let group: Placed[] = [], ends: number[] = [], groupEnd = -1
  const close = () => { for (const p of group) p.cols = ends.length; group = []; ends = [] }
  for (const p of xs) {
    if (p.top >= groupEnd) { close(); groupEnd = -1 }
    let col = ends.findIndex((end) => end <= p.top)
    if (col < 0) { col = ends.length; ends.push(p.bottom) } else ends[col] = p.bottom
    p.col = col; group.push(p); groupEnd = Math.max(groupEnd, p.bottom)
  }
  close()
  return xs
}
/** the hours the timelines of `days` show: [DAY_FROM_HOUR, DAY_TO_HOUR], widened to every event's whole hours */
export function hoursOf(placed: Placed[][]): [number, number] {
  let a = DAY_FROM_HOUR, b = DAY_TO_HOUR
  for (const p of placed.flat()) { a = Math.min(a, Math.floor(p.top / 60)); b = Math.max(b, Math.ceil(Math.min(p.bottom, 24 * 60) / 60)) }
  return [a, b]
}

/* ───────── deadlines on the calendar ───────── */
/** the local day of a todo's deadline in the calendar's zone: a day-only one as written, one with a time in `tz` */
export const todoDay = (t: Pick<Todo, "due" | "due_date">, tz: string) => t.due_date ?? (t.due != null ? dayIn(t.due, tz) : null)
export type DayMark = { kind: "todo"; todo: Todo; overdue: boolean } | { kind: "goal"; todo: Todo }
/** what the all-day row of `day` holds beside all-day events: open todos due that day, open goals whose target it is */
export function marksOn(todos: Todo[], day: string, tz: string, now: number): DayMark[] {
  const out: DayMark[] = []
  for (const t of todos) {
    if (t.state !== "open") continue
    if (t.kind === "goal") { if (t.target_date === day) out.push({ kind: "goal", todo: t }); continue }
    if (todoDay(t, tz) === day) out.push({ kind: "todo", todo: t, overdue: isOverdue(t, now) })
  }
  return out.sort((a, b) => (a.kind === b.kind ? 0 : a.kind === "goal" ? -1 : 1) || Number(b.todo.star) - Number(a.todo.star) || (a.todo.due ?? 0) - (b.todo.due ?? 0))
}

/* ───────── the day's summary (rules, no model) ───────── */
/** how long a free time the day's summary looks for */
export const FREE_MIN = 30
export type DaySummary = {
  meetings: number; conflicts: CalEvent[]; invites: CalEvent[]
  dueToday: Todo[]; overdue: Todo[]; goals: Todo[]
  /** nothing on: no meeting, nothing due or overdue */
  quiet: boolean
}
export function summarize(events: CalEvent[], todos: Todo[], day: string, tz: string, now: number): DaySummary {
  const today = events.filter((e) => onDay(e, day, tz))
  const meetings = today.filter(isMeeting)
  const conflicts = meetings.filter((e) => e.conflicts_with?.length)
  const invites = today.filter(awaitsReply)
  const open = todos.filter((t) => t.state === "open" && t.kind === "todo" && !(t.snooze_until != null && t.snooze_until > now))
  const dueToday = open.filter((t) => todoDay(t, tz) === day && !isOverdue(t, now))
  const overdue = open.filter((t) => isOverdue(t, now))
  const weekEnd = plusDays(mondayOf(day), 6)
  const goals = todos.filter((t) => t.kind === "goal" && t.state === "open" && t.target_date != null && t.target_date >= day && t.target_date <= weekEnd)
  return { meetings: meetings.length, conflicts, invites, dueToday, overdue, goals, quiet: !meetings.length && !invites.length && !dueToday.length && !overdue.length }
}
/** the meeting on now or next on `day`: one you have not declined, not cancelled, not over */
export function nextMeeting(events: CalEvent[], now: number): CalEvent | null {
  return events.filter((e) => isMeeting(e) && e.end_ms > now).sort((a, b) => a.start_ms - b.start_ms)[0] ?? null
}

/* ───────── the agenda ───────── */
/** the agenda's days with what is on each, empty days left out: all-day events first, then by start */
export function agendaDays(events: CalEvent[], days: string[], tz: string): [string, CalEvent[]][] {
  return days.map((d) => [d, events.filter((e) => e.status !== "cancelled" && onDay(e, d, tz)).sort((a, b) => Number(b.all_day) - Number(a.all_day) || a.start_ms - b.start_ms)] as [string, CalEvent[]])
    .filter(([, xs]) => xs.length > 0)
}

/* ───────── calendars ───────── */
/** a calendar's colour: the source's own, else the n-th of the series colours */
export const SERIES = 5
export const colorOf = (color: string | null | undefined, n: number) => (color && /^#[0-9a-f]{3,8}$/i.test(color) ? color : `var(--series-${(n % SERIES) + 1})`)

/** a meeting link the page may open: http(s) only, never a script or a file */
export const safeUrl = (u: string | null | undefined) => (u && /^https?:\/\/[^\s]+$/i.test(u.trim()) ? u.trim() : null)
