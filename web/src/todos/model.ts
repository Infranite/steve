/* 待办 (personal todos and goals, steve-digest `todos`): the shapes the Hub answers with, and the rules the pages
 * follow, as plain functions without React so node --test can check them.
 *
 * Days are the browser's local days. A day-only deadline (`due_date`) is a local date the Hub keeps as written; a
 * deadline with a time (`due`) is a moment, shown in the browser's time zone. */

export type TodoState = "open" | "done" | "dropped" | "handed"
/** a goal's progress as the Hub counts it: `pct` is null when nothing counts (not split yet, or all dropped) */
export type Progress = { done: number; open: number; dropped: number; total: number; pct: number | null }
export type Todo = {
  id: string; text: string; state: TodoState; item_id: string | null
  matter: { id: string; title: string | null } | null
  due: number | null; due_quote: string | null; due_kind: "time" | "date" | null; due_date: string | null
  source: "steve" | "you" | "agent"; writer: string; created: number; updated: number; closed: number | null
  kind: "todo" | "goal"; parent: { id: string; text: string | null } | null; target_date: string | null
  remind_at: number | null; snooze_until: number | null; block_event: string | null
  pri: number; star: boolean; recur: string | null; progress: Progress | null
  sources: { conv: string; message: string; name: string | null; href: string | null; excerpt: string | null }[]
  people: { id: string; name: string | null }[]
}
/** a goal read on its own: its child todos and the ids of the items linked to it */
export type Goal = Todo & { children: Todo[]; items: string[] }

const pad = (n: number) => String(n).padStart(2, "0")
/** the local day of a moment, "YYYY-MM-DD" */
export const dayOf = (ms: number | Date) => { const d = new Date(ms); return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}` }
/** a local day as the Date of its midnight */
export const dateOf = (day: string) => { const [y, m, d] = day.split("-").map(Number); return new Date(y, m - 1, d) }
/** the local day `n` days after `day` */
export const addDays = (day: string, n: number) => { const d = dateOf(day); d.setDate(d.getDate() + n); return dayOf(d) }
/** the last day (Sunday) of the week `day` is in; weeks start on Monday */
export const weekEnd = (day: string) => addDays(day, (7 - dateOf(day).getDay()) % 7)

/** the local day a todo is due on, if it has a deadline */
export const dueDay = (t: Pick<Todo, "due" | "due_date">) => t.due_date ?? (t.due != null ? dayOf(t.due) : null)
/** past its deadline: a day-only one once that day is over, one with a time once the time has passed */
export const isOverdue = (t: Pick<Todo, "due" | "due_date">, now: number) => t.due_date != null ? t.due_date < dayOf(now) : t.due != null && t.due < now
/** put off until later: snoozed to a moment not reached yet */
export const isSnoozed = (t: Pick<Todo, "snooze_until">, now: number) => t.snooze_until != null && t.snooze_until > now

export type Bucket = "overdue" | "today" | "week" | "later" | "none" | "snoozed" | "handed" | "closed"
export const BUCKETS: Bucket[] = ["overdue", "today", "week", "later", "none", "snoozed", "handed", "closed"]
/** which group of the todo page a (non-goal) todo is in */
export function bucketOf(t: Todo, now: number): Bucket {
  if (t.state === "handed") return "handed"
  if (t.state !== "open") return "closed"
  if (isSnoozed(t, now)) return "snoozed"
  const day = dueDay(t)
  if (day == null) return "none"
  if (isOverdue(t, now)) return "overdue"
  const today = dayOf(now)
  if (day <= today) return "today"
  return day <= weekEnd(today) ? "week" : "later"
}

/** how long a todo handed to a Bot stays in 最近交给 Bot on the 今天 view */
export const RECENT_HANDED_MS = 7 * 86_400_000
/** how many closed todos the 全部 view shows, newest first */
export const CLOSED_SHOWN = 30

export type View = "today" | "all"
/** the groups of the todo page, in order, without empty ones. 今天: what is due by the end of the week, what has no date,
 *  and what went to a Bot lately. 全部: every open todo, the snoozed ones, everything handed, and the latest closed. */
export function groups(todos: Todo[], view: View, now: number): [Bucket, Todo[]][] {
  const by = new Map<Bucket, Todo[]>()
  for (const t of todos) {
    if (t.kind !== "todo") continue
    const b = bucketOf(t, now)
    if (view === "today" && (b === "later" || b === "snoozed" || b === "closed")) continue
    if (view === "today" && b === "handed" && now - (t.closed ?? t.updated) > RECENT_HANDED_MS) continue
    by.set(b, [...(by.get(b) ?? []), t])
  }
  return BUCKETS.filter((b) => by.has(b)).map((b) => {
    const xs = by.get(b)!
    if (b === "closed" || b === "handed") return [b, [...xs].sort((x, y) => (y.closed ?? y.updated) - (x.closed ?? x.updated)).slice(0, b === "closed" ? CLOSED_SHOWN : undefined)]
    if (b === "snoozed") return [b, [...xs].sort((x, y) => x.snooze_until! - y.snooze_until!)]
    return [b, [...xs].sort(byUrgency)]
  })
}
/** starred first, then the higher priority, the earlier deadline, the newer */
export function byUrgency(a: Todo, b: Todo) {
  const due = (t: Todo) => t.due ?? (t.due_date ? dateOf(t.due_date).getTime() + 86_399_999 : Infinity)
  return Number(b.star) - Number(a.star) || b.pri - a.pri || due(a) - due(b) || b.created - a.created
}

/** the sidebar's gray count: open todos due today or overdue, not snoozed */
export const dueCount = (todos: Todo[], now: number) =>
  todos.filter((t) => t.kind === "todo" && t.state === "open" && !isSnoozed(t, now) && dueDay(t) != null && (isOverdue(t, now) || dueDay(t)! <= dayOf(now))).length

/** a deadline in words, as parts the page translates: the day relative to today, and the time when it has one */
export type DueWhen = { rel: "today" | "tomorrow" | "yesterday" | "week" | "date"; day: string; time: string | null; overdue: boolean }
export function dueWhen(t: Pick<Todo, "due" | "due_date">, now: number): DueWhen | null {
  const day = dueDay(t)
  if (day == null) return null
  const today = dayOf(now), d = new Date(t.due ?? 0)
  const rel = day === today ? "today" : day === addDays(today, 1) ? "tomorrow" : day === addDays(today, -1) ? "yesterday" : day > today && day <= weekEnd(today) ? "week" : "date"
  return { rel, day, time: t.due_date == null && t.due != null ? `${pad(d.getHours())}:${pad(d.getMinutes())}` : null, overdue: isOverdue(t, now) }
}

/** the open todos with a deadline in the next `days` days (overdue ones too), by deadline: what the calendar page lists
 *  before a calendar is connected */
export const upcoming = (todos: Todo[], now: number, days = 7) => {
  const end = addDays(dayOf(now), days - 1)
  return todos.filter((t) => t.kind === "todo" && t.state === "open" && dueDay(t) != null && dueDay(t)! <= end)
    .sort((a, b) => dueDay(a)!.localeCompare(dueDay(b)!) || (a.due ?? 0) - (b.due ?? 0))
}

/* ───────── snooze ───────── */
/** the moments a todo can be snoozed to: this evening (when it is still ahead), tomorrow morning, next Monday morning.
 *  `morning` and `evening` are local "HH:MM" */
export function snoozeChoices(now: number, morning = "09:00", evening = "18:00"): { key: "evening" | "tomorrow" | "monday"; at: number }[] {
  const at = (day: string, hm: string) => { const d = dateOf(day); const [h, m] = hm.split(":").map(Number); d.setHours(h, m, 0, 0); return d.getTime() }
  const today = dayOf(now), out: { key: "evening" | "tomorrow" | "monday"; at: number }[] = []
  if (at(today, evening) > now) out.push({ key: "evening", at: at(today, evening) })
  out.push({ key: "tomorrow", at: at(addDays(today, 1), morning) })
  out.push({ key: "monday", at: at(addDays(weekEnd(today), 1), morning) })
  return out
}

/* ───────── quick add, read by fixed rules ─────────
 * "周五前 交 Q4 复盘 18:00 #Q4复盘 !2": a day, a time, a goal and a priority are taken out of the text and shown as
 * chips; a removed chip puts its words back. Nothing is guessed by a model, and what is not recognised stays text. */
export type Token = { key: string; kind: "day" | "time" | "goal" | "pri"; start: number; end: number; raw: string; day?: string; time?: string; goal?: { id: string; text: string }; pri?: number }
export type Quick = { text: string; tokens: Token[]; due?: number; due_date?: string; parent?: { id: string; text: string }; pri?: number }

const ZH_DAY: Record<string, number> = { 一: 1, 二: 2, 三: 3, 四: 4, 五: 5, 六: 6, 日: 0, 天: 0, "1": 1, "2": 2, "3": 3, "4": 4, "5": 5, "6": 6, "7": 0 }
const EN_DAY: Record<string, number> = { mon: 1, tue: 2, wed: 3, thu: 4, fri: 5, sat: 6, sun: 0 }
/** the next `dow` on or after `today`; with `nextWeek`, that weekday in the week after this one */
function weekday(today: string, dow: number, nextWeek: boolean) {
  if (nextWeek) { const monday = addDays(weekEnd(today), 1); return addDays(monday, (dow + 6) % 7) }
  return addDays(today, (dow - dateOf(today).getDay() + 7) % 7)
}
/** a month and day without a year: this year's, or next year's when it has passed */
function monthDay(today: string, m: number, d: number) {
  const y = Number(today.slice(0, 4)), day = `${y}-${pad(m)}-${pad(d)}`
  if (!validDay(day)) return null
  return day < today ? `${y + 1}-${pad(m)}-${pad(d)}` : day
}
const validDay = (day: string) => { const d = dateOf(day); return !Number.isNaN(d.getTime()) && dayOf(d) === day }
const hour12 = (h: number, part: string | undefined) => (part && /下午|晚上|pm/i.test(part) && h < 12 ? h + 12 : part && /中午/.test(part) && h < 11 ? h + 12 : part && /am/i.test(part) && h === 12 ? 0 : h)

type Rule = { kind: Token["kind"]; re: RegExp; read: (m: RegExpExecArray, today: string) => Partial<Token> | null }
const RULES: Rule[] = [
  { kind: "day", re: /(?<![\d-])(\d{4})-(\d{1,2})-(\d{1,2})(?![\d])(之前|前)?/g, read: (m) => { const d = `${m[1]}-${pad(+m[2])}-${pad(+m[3])}`; return validDay(d) ? { day: d } : null } },
  { kind: "day", re: /(\d{1,2})月(\d{1,2})[日号](之前|前)?/g, read: (m, today) => { const d = monthDay(today, +m[1], +m[2]); return d ? { day: d } : null } },
  { kind: "day", re: /(?<![\d:/-])(\d{1,2})[-/](\d{1,2})(?![\d:/-])(之前|前)?/g, read: (m, today) => { const d = monthDay(today, +m[1], +m[2]); return d ? { day: d } : null } },
  { kind: "day", re: /(今天|今日|今晚|明天|明日|明晚|后天)(之前|前)?/g, read: (m, today) => ({ day: addDays(today, /今/.test(m[1]) ? 0 : /明/.test(m[1]) ? 1 : 2) }) },
  { kind: "day", re: /(下个?|这|本)?(周|星期|礼拜)([一二三四五六日天1-7])(之前|前)?/g, read: (m, today) => ({ day: weekday(today, ZH_DAY[m[3]], !!m[1] && m[1].startsWith("下")) }) },
  { kind: "day", re: /\b(?:by\s+)?(today|tonight|tomorrow|tmr)\b/gi, read: (m, today) => ({ day: addDays(today, /^to(day|night)$/i.test(m[1]) ? 0 : 1) }) },
  { kind: "day", re: /\b(?:by\s+)?(next\s+)?(mon|tue|wed|thu|fri|sat|sun)(?:day|sday|nesday|rsday|urday)?\b/gi, read: (m, today) => ({ day: weekday(today, EN_DAY[m[2].toLowerCase()], !!m[1]) }) },
  { kind: "time", re: /(上午|早上|中午|下午|晚上)?\s?(?<!\d)([01]?\d|2[0-3])[:：]([0-5]\d)(?!\d)(之前|前)?/g, read: (m) => ({ time: `${pad(hour12(+m[2], m[1]))}:${m[3]}` }) },
  { kind: "time", re: /(上午|早上|中午|下午|晚上)?(?<!\d)([01]?\d|2[0-3])点(半|([0-5]?\d)分?)?(之前|前)?/g, read: (m) => ({ time: `${pad(hour12(+m[2], m[1]))}:${m[3] === "半" ? "30" : pad(+(m[4] ?? 0))}` }) },
  { kind: "time", re: /\b(1[0-2]|0?[1-9])(?::([0-5]\d))?\s?(am|pm)\b/gi, read: (m) => ({ time: `${pad(hour12(+m[1], m[3]))}:${m[2] ?? "00"}` }) },
  { kind: "pri", re: /(?<=^|\s)!([1-3])(?=\s|$)/g, read: (m) => ({ pri: +m[1] }) },
]
const squash = (s: string) => s.toLowerCase().replace(/\s+/g, "")

/** reads `input` at `now`. `goals`: the person's open goals, matched by `#words` (the first whose text holds them);
 *  `skip`: the keys of chips that were removed. Only the first day, time, goal and priority count. */
export function parseQuick(input: string, now: number, goals: { id: string; text: string }[] = [], skip: ReadonlySet<string> = new Set()): Quick {
  const today = dayOf(now), found: Token[] = []
  const free = (s: number, e: number) => found.every((x) => e <= x.start || s >= x.end)
  for (const r of RULES) {
    r.re.lastIndex = 0
    for (let m = r.re.exec(input); m; m = r.re.exec(input)) {
      const start = m.index, end = start + m[0].length
      if (!m[0].trim() || !free(start, end)) continue
      const v = r.read(m, today)
      if (v) found.push({ key: `${r.kind}:${start}:${m[0]}`, kind: r.kind, start, end, raw: m[0], ...v })
    }
  }
  for (const m of input.matchAll(/(?<=^|\s)#(\S+)/g)) {
    const start = m.index!, end = start + m[0].length, q = squash(m[1])
    const goal = goals.find((g) => squash(g.text).includes(q))
    if (goal && free(start, end)) found.push({ key: `goal:${start}:${m[0]}`, kind: "goal", start, end, raw: m[0], goal })
  }
  const kept: Token[] = []
  for (const x of found.sort((a, b) => a.start - b.start)) if (!skip.has(x.key) && !kept.some((k) => k.kind === x.kind)) kept.push(x)
  let text = "", at = 0
  for (const x of kept) { text += input.slice(at, x.start) + " "; at = x.end }
  text = (text + input.slice(at)).replace(/\s+/g, " ").trim()
  const day = kept.find((x) => x.kind === "day")?.day, time = kept.find((x) => x.kind === "time")?.time
  const q: Quick = { text, tokens: kept }
  if (time) {
    const d = dateOf(day ?? today); const [h, m] = time.split(":").map(Number); d.setHours(h, m, 0, 0); q.due = d.getTime()
  } else if (day) q.due_date = day
  const goal = kept.find((x) => x.kind === "goal")?.goal
  if (goal) q.parent = goal
  const pri = kept.find((x) => x.kind === "pri")?.pri
  if (pri) q.pri = pri
  return q
}

/** the repeat choices the detail page offers, as the Hub stores them */
export const RECURS = ["FREQ=DAILY", "FREQ=WEEKLY", "FREQ=MONTHLY", "FREQ=YEARLY"] as const
