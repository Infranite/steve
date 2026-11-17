import assert from "node:assert/strict"
import { test } from "node:test"
import { addDays, bucketOf, dayOf, dueCount, dueWhen, groups, parseQuick, snoozeChoices, upcoming, weekEnd } from "../src/todos/model.ts"

/* Wednesday 18 November 2026, 10:00 local time: every date below is a local day, synthetic */
const NOW = new Date(2026, 10, 18, 10, 0).getTime()
const at = (d, h = 0, m = 0) => new Date(2026, 10, d, h, m).getTime()
let n = 0
const todo = (extra = {}) => ({
  id: "td" + ++n, text: "todo " + n, state: "open", item_id: null, matter: null, due: null, due_quote: null, due_kind: null, due_date: null,
  source: "you", writer: "u1", created: NOW - n * 1000, updated: NOW - n * 1000, closed: null, kind: "todo", parent: null, target_date: null,
  remind_at: null, snooze_until: null, block_event: null, pri: 0, star: false, recur: null, progress: null, sources: [], people: [], ...extra,
})

test("weeks start on Monday and end on Sunday", () => {
  assert.equal(weekEnd("2026-11-18"), "2026-11-22")
  assert.equal(weekEnd("2026-11-22"), "2026-11-22")
  assert.equal(weekEnd("2026-11-23"), "2026-11-29")
  assert.equal(addDays("2026-12-31", 1), "2027-01-01")
})

test("a todo's group follows its deadline, its state and its snooze", () => {
  assert.equal(bucketOf(todo({ due_date: "2026-11-17" }), NOW), "overdue")
  assert.equal(bucketOf(todo({ due: at(18, 9) }), NOW), "overdue", "a time already passed today")
  assert.equal(bucketOf(todo({ due: at(18, 18) }), NOW), "today")
  assert.equal(bucketOf(todo({ due_date: "2026-11-18" }), NOW), "today", "a day-only deadline is not overdue on its day")
  assert.equal(bucketOf(todo({ due_date: "2026-11-22" }), NOW), "week")
  assert.equal(bucketOf(todo({ due_date: "2026-11-23" }), NOW), "later")
  assert.equal(bucketOf(todo(), NOW), "none")
  assert.equal(bucketOf(todo({ due_date: "2026-11-17", snooze_until: at(19, 9) }), NOW), "snoozed")
  assert.equal(bucketOf(todo({ due_date: "2026-11-17", snooze_until: at(18, 9) }), NOW), "overdue", "a snooze that ran out")
  assert.equal(bucketOf(todo({ state: "handed", item_id: "ST-1" }), NOW), "handed")
  assert.equal(bucketOf(todo({ state: "dropped" }), NOW), "closed")
})

test("今天 shows this week, no date and lately handed; 全部 shows everything; goals are not rows", () => {
  const xs = [
    todo({ id: "late", due_date: "2026-11-23" }), todo({ id: "nodate" }), todo({ id: "over", due_date: "2026-11-10" }),
    todo({ id: "snz", snooze_until: at(20) }), todo({ id: "old-hand", state: "handed", item_id: "ST-1", closed: NOW - 8 * 86_400_000 }),
    todo({ id: "new-hand", state: "handed", item_id: "ST-2", closed: NOW - 86_400_000 }), todo({ id: "done", state: "done", closed: NOW - 1000 }),
    todo({ id: "goal", kind: "goal", target_date: "2026-12-31" }),
  ]
  assert.deepEqual(groups(xs, "today", NOW).map(([b, ys]) => [b, ys.map((y) => y.id)]), [["overdue", ["over"]], ["none", ["nodate"]], ["handed", ["new-hand"]]])
  assert.deepEqual(groups(xs, "all", NOW).map(([b, ys]) => [b, ys.map((y) => y.id)]),
    [["overdue", ["over"]], ["later", ["late"]], ["none", ["nodate"]], ["snoozed", ["snz"]], ["handed", ["new-hand", "old-hand"]], ["closed", ["done"]]])
})

test("within a group: starred, then higher priority, then the earlier deadline", () => {
  const xs = [todo({ id: "a", due_date: "2026-11-18" }), todo({ id: "b", due: at(18, 12) }), todo({ id: "c", due: at(18, 23), pri: 2 }), todo({ id: "d", due: at(18, 23), star: true })]
  assert.deepEqual(groups(xs, "today", NOW)[0][1].map((x) => x.id), ["d", "c", "b", "a"])
})

test("the sidebar counts open todos due today or overdue, not snoozed ones or goals", () => {
  const xs = [todo({ due_date: "2026-11-18" }), todo({ due: at(17) }), todo({ due_date: "2026-11-19" }), todo(), todo({ due_date: "2026-11-01", snooze_until: at(19) }),
    todo({ due_date: "2026-11-01", state: "done" }), todo({ kind: "goal", target_date: "2026-11-01" })]
  assert.equal(dueCount(xs, NOW), 2)
})

test("a deadline in words is relative to today", () => {
  assert.deepEqual(dueWhen({ due: at(18, 18), due_date: null }, NOW), { rel: "today", day: "2026-11-18", time: "18:00", overdue: false })
  assert.deepEqual(dueWhen({ due: null, due_date: "2026-11-19" }, NOW), { rel: "tomorrow", day: "2026-11-19", time: null, overdue: false })
  assert.equal(dueWhen({ due: null, due_date: "2026-11-21" }, NOW).rel, "week")
  assert.equal(dueWhen({ due: null, due_date: "2026-11-23" }, NOW).rel, "date")
  assert.deepEqual(dueWhen({ due: null, due_date: "2026-11-17" }, NOW), { rel: "yesterday", day: "2026-11-17", time: null, overdue: true })
  assert.equal(dueWhen({ due: null, due_date: null }, NOW), null)
})

test("quick add reads a day, a time, a goal and a priority by fixed rules", () => {
  const goals = [{ id: "g1", text: "Q4 复盘" }, { id: "g2", text: "Hub 协调器上线" }]
  const q = parseQuick("周五前 交 Q4 复盘 18:00 #Q4复盘 !2", NOW, goals)
  assert.equal(q.text, "交 Q4 复盘")
  assert.equal(q.due, at(20, 18))
  assert.equal(q.due_date, undefined)
  assert.deepEqual(q.parent, goals[0])
  assert.equal(q.pri, 2)
  assert.deepEqual(q.tokens.map((x) => x.kind), ["day", "time", "goal", "pri"])
  assert.deepEqual(parseQuick("明天 写周报", NOW), { text: "写周报", tokens: parseQuick("明天 写周报", NOW).tokens, due_date: "2026-11-19" })
  assert.equal(parseQuick("下周一 交材料", NOW).due_date, "2026-11-23")
  assert.equal(parseQuick("周三 例会", NOW).due_date, "2026-11-18", "today's weekday is today")
  assert.equal(parseQuick("12月3日前 续费", NOW).due_date, "2026-12-03")
  assert.equal(parseQuick("1-5 年假", NOW).due_date, "2027-01-05", "a month and day already past is next year's")
  assert.equal(parseQuick("2026-11-30 结账", NOW).due_date, "2026-11-30")
  assert.equal(parseQuick("下午3点半 打电话", NOW).due, at(18, 15, 30), "a time alone is today's")
  assert.equal(parseQuick("send it by fri 6pm", NOW).due, at(20, 18))
  assert.equal(parseQuick("send it by fri 6pm", NOW).text, "send it")
  assert.equal(parseQuick("call tomorrow", NOW).due_date, "2026-11-19")
})

test("quick add leaves what it does not recognise, and a removed chip, as text", () => {
  const q = parseQuick("换掉家里的路由器 #没有这个目标", NOW, [{ id: "g1", text: "Q4 复盘" }])
  assert.equal(q.text, "换掉家里的路由器 #没有这个目标")
  assert.equal(q.parent, undefined)
  assert.equal(parseQuick("2026-02-30 不存在的日子", NOW).due_date, undefined)
  const first = parseQuick("明天 看完 周报", NOW)
  const skipped = parseQuick("明天 看完 周报", NOW, [], new Set([first.tokens[0].key]))
  assert.equal(skipped.text, "明天 看完 周报")
  assert.equal(skipped.due_date, undefined)
  assert.equal(parseQuick("今天 明天", NOW).due_date, "2026-11-18", "only the first day counts")
})

test("snooze offers this evening only while it is ahead, then tomorrow and next Monday mornings", () => {
  assert.deepEqual(snoozeChoices(NOW).map((c) => [c.key, dayOf(c.at), new Date(c.at).getHours()]), [["evening", "2026-11-18", 18], ["tomorrow", "2026-11-19", 9], ["monday", "2026-11-23", 9]])
  assert.deepEqual(snoozeChoices(at(18, 20)).map((c) => c.key), ["tomorrow", "monday"])
})

test("the calendar page lists open todos due within the days it shows, overdue first", () => {
  const xs = [todo({ id: "in7", due_date: "2026-11-24" }), todo({ id: "out", due_date: "2026-11-25" }), todo({ id: "over", due_date: "2026-11-02" }), todo({ id: "today", due: at(18, 12) }), todo({ id: "nodate" })]
  assert.deepEqual(upcoming(xs, NOW, 7).map((x) => x.id), ["over", "today", "in7"])
})
