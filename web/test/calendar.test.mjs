import assert from "node:assert/strict"
import { test } from "node:test"
import {
  agendaDays, awaitsReply, dayIn, daysBetween, daysOf, eventDays, hmIn, hoursOf, isDay, isoWeekday, layoutDay, marksOn,
  minutesOn, mondayOf, sameClock, monthGrid, nextMeeting, plusDays, rangeOf, safeUrl, step, summarize, todoDay, colorOf,
} from "../src/calendar/model.ts"

/* every event and todo below is synthetic; the reader's zone is Asia/Shanghai (UTC+8, no daylight saving) unless a
 * test says otherwise. Wednesday 18 November 2026, 10:00 there. */
const TZ = "Asia/Shanghai"
const at = (d, h = 0, m = 0) => Date.UTC(2026, 10, d, h - 8, m)
const NOW = at(18, 10)
let n = 0
const ev = (start, end, extra = {}) => ({
  id: "ev" + ++n, meeting_id: null, calendar: { id: "cal1", name: "Work" }, title: "Event " + n, all_day: false,
  start_ms: start, end_ms: end, start: "", end: "", status: "confirmed", busy_only: false, version: 1, ...extra,
})
const allDay = (from, to, extra = {}) => ev(0, 0, { all_day: true, start: from, end: to, ...extra })
const todo = (extra = {}) => ({
  id: "td" + ++n, text: "todo " + n, state: "open", item_id: null, matter: null, due: null, due_quote: null, due_kind: null, due_date: null,
  source: "you", writer: "u1", created: 0, updated: 0, closed: null, kind: "todo", parent: null, target_date: null,
  remind_at: null, snooze_until: null, block_event: null, pri: 0, star: false, recur: null, progress: null, sources: [], people: [], ...extra,
})

test("days and times are read in the reader's zone, not the browser's", () => {
  /* 23:30 UTC on the 17th is already the 18th in Shanghai and still the 17th in New York */
  const t = Date.UTC(2026, 10, 17, 23, 30)
  assert.equal(dayIn(t, TZ), "2026-11-18")
  assert.equal(hmIn(t, TZ), "07:30")
  assert.equal(dayIn(t, "America/New_York"), "2026-11-17")
  assert.equal(hmIn(t, "America/New_York"), "18:30")
  /* midnight is 00:00, never 24:00 */
  assert.equal(hmIn(at(19, 0), TZ), "00:00")
  assert.equal(sameClock(t, TZ, "Asia/Taipei"), true)
  assert.equal(sameClock(t, TZ, "America/New_York"), false)
  /* an unknown zone falls back to UTC instead of throwing */
  assert.equal(dayIn(t, "Not/AZone"), "2026-11-17")
})

test("day arithmetic is on the calendar: months, years, weekdays, a six-week month", () => {
  assert.equal(plusDays("2026-12-31", 1), "2027-01-01")
  assert.equal(plusDays("2026-03-01", -1), "2026-02-28")
  assert.equal(daysBetween("2026-11-16", "2026-11-22"), 6)
  assert.equal(isoWeekday("2026-11-18"), 3)
  assert.equal(isoWeekday("2026-11-22"), 7)
  assert.equal(mondayOf("2026-11-22"), "2026-11-16")
  assert.equal(mondayOf("2026-11-16"), "2026-11-16")
  const g = monthGrid("2026-11-18")
  assert.equal(g.length, 6)
  assert.deepEqual([g[0][0], g[5][6]], ["2026-10-26", "2026-12-06"])
  assert.ok(isDay("2026-02-28") && !isDay("2026-02-30") && !isDay("2026-11-18T10:00") && !isDay(null))
})

test("each view reads whole days around its day; back and forth move by its length", () => {
  assert.deepEqual(rangeOf("day", "2026-11-18"), { from: "2026-11-18", to: "2026-11-18" })
  assert.deepEqual(rangeOf("week", "2026-11-18"), { from: "2026-11-16", to: "2026-11-22" })
  assert.deepEqual(rangeOf("three", "2026-11-18"), { from: "2026-11-18", to: "2026-11-20" })
  assert.deepEqual(rangeOf("agenda", "2026-11-18"), { from: "2026-11-18", to: "2026-12-01" })
  assert.equal(daysOf("week", "2026-11-18").length, 7)
  assert.equal(step("day", "2026-11-18", -1), "2026-11-17")
  assert.equal(step("week", "2026-11-18", 1), "2026-11-23")
  assert.equal(step("three", "2026-11-18", 1), "2026-11-21")
  assert.equal(step("agenda", "2026-11-18", -1), "2026-11-11")
})

test("an event's days and minutes: clipped at midnight, an end at midnight stays on its day", () => {
  const night = ev(at(18, 22), at(19, 2))
  assert.deepEqual(eventDays(night, TZ), ["2026-11-18", "2026-11-19"])
  assert.deepEqual(minutesOn(night, "2026-11-18", TZ), [22 * 60, 24 * 60, false, true])
  assert.deepEqual(minutesOn(night, "2026-11-19", TZ), [0, 2 * 60, true, false])
  const late = ev(at(18, 23), at(19, 0))
  assert.deepEqual(eventDays(late, TZ), ["2026-11-18", "2026-11-18"])
  assert.deepEqual(minutesOn(late, "2026-11-18", TZ), [23 * 60, 24 * 60, false, false])
  /* an all-day event is on its dates, the last one inclusive */
  assert.deepEqual(eventDays(allDay("2026-11-20", "2026-11-21"), TZ), ["2026-11-20", "2026-11-21"])
})

test("overlapping events share the width; the next group starts over", () => {
  const a = ev(at(18, 14), at(18, 15), { id: "a" }), b = ev(at(18, 14, 30), at(18, 15), { id: "b" })
  const c = ev(at(18, 14, 45), at(18, 16), { id: "c" }), d = ev(at(18, 16), at(18, 17), { id: "d" })
  const e = ev(at(18, 9), at(18, 9, 5), { id: "e" }), x = ev(at(18, 11), at(18, 12), { id: "x", status: "cancelled" })
  const p = Object.fromEntries(layoutDay([d, c, b, a, e, x], "2026-11-18", TZ).map((q) => [q.e.id, q]))
  assert.equal(p.x, undefined, "a cancelled event is not drawn")
  assert.deepEqual([p.a.col, p.b.col, p.c.col], [0, 1, 2])
  assert.deepEqual([p.a.cols, p.b.cols, p.c.cols], [3, 3, 3])
  assert.deepEqual([p.d.col, p.d.cols], [0, 1], "starts when c ends: a group of its own")
  assert.deepEqual([p.e.top, p.e.bottom], [9 * 60, 9 * 60 + 22], "a short event still gets room")
  /* a column freed inside a group is used again */
  const f = ev(at(18, 10), at(18, 11), { id: "f" }), g = ev(at(18, 10), at(18, 10, 30), { id: "g" }), h = ev(at(18, 10, 30), at(18, 11), { id: "h" })
  const q = Object.fromEntries(layoutDay([f, g, h], "2026-11-18", TZ).map((y) => [y.e.id, y]))
  assert.deepEqual([q.f.col, q.g.col, q.h.col, q.h.cols], [0, 1, 1, 2])
})

test("the timeline shows 08:00–20:00, widened to every event's whole hours", () => {
  assert.deepEqual(hoursOf([[]]), [8, 20])
  const early = layoutDay([ev(at(18, 6, 30), at(18, 7)), ev(at(18, 21), at(18, 22, 15))], "2026-11-18", TZ)
  assert.deepEqual(hoursOf([early]), [6, 23])
})

test("the all-day row: todos due that day in the reader's zone, overdue marked, goals' target days first", () => {
  const timed = todo({ due: Date.UTC(2026, 10, 17, 23, 0) }) /* 07:00 on the 18th in Shanghai */
  const dated = todo({ due_date: "2026-11-18", star: true })
  const goal = todo({ kind: "goal", target_date: "2026-11-18" })
  const done = todo({ due_date: "2026-11-18", state: "done" })
  const other = todo({ due_date: "2026-11-19" })
  assert.equal(todoDay(timed, TZ), "2026-11-18")
  assert.equal(todoDay(timed, "America/New_York"), "2026-11-17")
  const m = marksOn([timed, dated, goal, done, other], "2026-11-18", TZ, NOW)
  assert.deepEqual(m.map((x) => [x.kind, x.todo.id]), [["goal", goal.id], ["todo", dated.id], ["todo", timed.id]])
  assert.equal(m[2].overdue, true, "07:00 has passed at 10:00")
})

test("the day's summary counts by rules and is quiet when nothing is on", () => {
  const a = ev(at(18, 14), at(18, 15), { id: "a", conflicts_with: ["b"] }), b = ev(at(18, 14, 30), at(18, 15), { id: "b", conflicts_with: ["a"] })
  const inv = ev(at(18, 16), at(18, 17), { id: "inv", my_rsvp: "needs_action" })
  const no = ev(at(18, 11), at(18, 12), { id: "no", my_rsvp: "declined" })
  const holiday = allDay("2026-11-18", "2026-11-18")
  const tomorrow = ev(at(19, 9), at(19, 10))
  const todos = [todo({ due_date: "2026-11-18" }), todo({ due_date: "2026-11-14" }), todo({ due_date: "2026-11-18", snooze_until: at(19, 9) }),
    todo({ kind: "goal", target_date: "2026-11-20" }), todo({ kind: "goal", target_date: "2026-12-20" })]
  const s = summarize([a, b, inv, no, holiday, tomorrow], todos, "2026-11-18", TZ, NOW)
  assert.equal(s.meetings, 3, "declined and all-day ones are no meetings")
  assert.deepEqual(s.conflicts.map((e) => e.id), ["a", "b"])
  assert.deepEqual(s.invites.map((e) => e.id), ["inv"])
  assert.deepEqual([s.dueToday.length, s.overdue.length, s.goals.length, s.quiet], [1, 1, 1, false])
  assert.equal(awaitsReply(inv), true)
  assert.equal(summarize([tomorrow, holiday], [], "2026-11-18", TZ, NOW).quiet, true)
})

test("the next meeting: on now or next, never a declined, cancelled or finished one", () => {
  const past = ev(at(18, 8), at(18, 9)), on = ev(at(18, 9, 30), at(18, 10, 30), { id: "on" })
  const no = ev(at(18, 10, 15), at(18, 11), { my_rsvp: "declined" }), later = ev(at(18, 11), at(18, 12), { id: "later" })
  assert.equal(nextMeeting([later, no, on, past], NOW).id, "on")
  assert.equal(nextMeeting([later, no, past], NOW).id, "later")
  assert.equal(nextMeeting([past], NOW), null)
})

test("the agenda lists days with something, all-day first, cancelled left out", () => {
  const a = ev(at(18, 15), at(18, 16), { id: "a" }), b = ev(at(18, 9), at(18, 10), { id: "b" })
  const c = allDay("2026-11-18", "2026-11-19", { id: "c" }), x = ev(at(20, 9), at(20, 10), { status: "cancelled" })
  const days = agendaDays([a, b, c, x], daysOf("three", "2026-11-18"), TZ)
  assert.deepEqual(days.map(([d, xs]) => [d, xs.map((e) => e.id)]), [["2026-11-18", ["c", "b", "a"]], ["2026-11-19", ["c"]]])
})

test("colours and links", () => {
  assert.equal(colorOf("#3e63dd", 0), "#3e63dd")
  assert.equal(colorOf(null, 0), "var(--series-1)")
  assert.equal(colorOf("red; background: url(x)", 6), "var(--series-2)")
  assert.equal(safeUrl("https://meet.example/abc"), "https://meet.example/abc")
  assert.equal(safeUrl("javascript:alert(1)"), null)
  assert.equal(safeUrl("file:///etc/passwd"), null)
  assert.equal(safeUrl(null), null)
})
