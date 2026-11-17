/* The pieces the todo, goal and calendar pages share: quick add, a todo's row, a goal's card and progress, a deadline
 * in words, and the actions on a todo (done, snooze, hand to a Bot, drop). */
import { useMemo, useRef, useState, type ReactNode } from "react"
import { BotIcon, CalendarClockIcon, ChevronDownIcon, CircleSlashIcon, MoreHorizontalIcon, PlusIcon, RepeatIcon, SparklesIcon, StarIcon, TargetIcon, UndoIcon, XIcon } from "lucide-react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Progress } from "@/components/ui/progress"
import { t as t_, useT } from "@/i18n"
import { monthDayShort, weekday } from "@/i18n/format"
import { newItem, st, toast } from "@/store"
import { Card, go, Status } from "@/ui"
import { change, todosApi } from "./data"
import { dateOf, dayOf, dueWhen, isSnoozed, parseQuick, snoozeChoices, type Goal, type Todo, type Token } from "./model"

/* ───────── a deadline in words ───────── */
/** "18:00", "明天", "周五 18:00", "11-20": the deadline relative to today, red once it has passed */
export function useDueText() {
  const t = useT()
  return (x: Pick<Todo, "due" | "due_date">, now: number) => {
    const w = dueWhen(x, now)
    if (!w) return null
    const d = dateOf(w.day)
    if (w.time) {
      if (w.rel === "today") return w.time
      if (w.rel === "tomorrow") return t("common:time.tomorrow", { time: w.time })
      if (w.rel === "yesterday") return t("common:time.yesterday", { time: w.time })
      return w.rel === "week" ? t("common:time.weekdayAt", { weekday: weekday(d), time: w.time }) : t("common:time.dateAt", { date: monthDayShort(d), time: w.time })
    }
    return w.rel === "today" ? t("common:time.todayDay") : w.rel === "tomorrow" ? t("todos:due.tomorrow") : w.rel === "yesterday" ? t("common:time.yesterdayDay") : w.rel === "week" ? weekday(d) : monthDayShort(d)
  }
}
/** a moment as the same words (a snooze, a reminder) */
const at = (ms: number) => ({ due: ms, due_date: null })

/* ───────── quick add ───────── */
/** One line that becomes a todo on Enter. Fixed rules read a day ("周五前", "明天", "11-20"), a time ("18:00",
 *  "下午3点"), a goal ("#Q4复盘") and a priority ("!2") out of it, shown as chips under the field; removing a chip keeps
 *  those words in the text. `parent`: the goal every todo added here goes under. */
export function QuickAdd({ goals, parent, kind = "todo", autoFocus, className, placeholder }: { goals: Todo[]; parent?: string; kind?: "todo" | "goal"; autoFocus?: boolean; className?: string; placeholder?: string }) {
  const t = useT(), due = useDueText()
  const [text, setText] = useState(""), [skip, setSkip] = useState<Set<string>>(new Set()), [busy, setBusy] = useState(false)
  const ref = useRef<HTMLInputElement>(null)
  const now = Date.now()
  const open = useMemo(() => goals.filter((g) => g.kind === "goal" && g.state === "open").map((g) => ({ id: g.id, text: g.text })), [goals])
  const q = useMemo(() => kind === "goal" ? { text: text.trim(), tokens: [] as Token[] } : parseQuick(text, now, parent ? [] : open, skip), [text, skip, open, parent, kind]) // eslint-disable-line react-hooks/exhaustive-deps
  const chip = (x: Token): ReactNode => {
    if (x.kind === "goal") return t("todos:quick.goal", { goal: x.goal!.text })
    if (x.kind === "pri") return t("todos:quick.pri", { pri: t(`todos:pri.${x.pri as 1 | 2 | 3}`) })
    if (x.kind === "time" && "due" in q && q.due != null) return t("todos:quick.due", { when: due(at(q.due), now) })
    if (x.kind === "day" && "due_date" in q && q.due_date) return t("todos:quick.due", { when: due({ due: null, due_date: q.due_date }, now) })
    return null
  }
  /* a day and a time make one deadline: one chip, and removing it removes both */
  const tokens = q.tokens.filter((x) => !(x.kind === "day" && q.tokens.some((y) => y.kind === "time")))
  const remove = (x: Token) => setSkip(new Set([...skip, x.key, ...(x.kind === "time" ? q.tokens.filter((y) => y.kind === "day").map((y) => y.key) : [])]))
  const add = async () => {
    if (!q.text || busy) return
    setBusy(true)
    const p = "parent" in q ? q.parent : undefined
    const r = await change(todosApi.add({
      text: q.text, kind, ...(parent ? { parent } : p ? { parent: p.id } : {}),
      ...("due" in q && q.due != null ? { due: q.due } : {}), ...("due_date" in q && q.due_date ? { due_date: q.due_date } : {}), ...("pri" in q && q.pri ? { pri: q.pri } : {}),
    }), kind === "goal" ? t("todos:toast.goalAdded") : t("todos:toast.added"))
    setBusy(false)
    if (r) { setText(""); setSkip(new Set()); ref.current?.focus() }
  }
  return (
    <div className={cn("min-w-0", className)} data-quick-add={kind}>
      <form onSubmit={(e) => { e.preventDefault(); void add() }} className="flex h-11 items-center gap-2 rounded-xl bg-card px-3 shadow-[var(--lift)] focus-within:ring-[3px] focus-within:ring-ring/40">
        <PlusIcon className="size-4 shrink-0 text-tertiary" aria-hidden />
        <input ref={ref} value={text} autoFocus={autoFocus} onChange={(e) => { setText(e.target.value); if (!e.target.value) setSkip(new Set()) }}
          placeholder={placeholder ?? (kind === "goal" ? t("todos:quick.goalPlaceholder") : t("todos:quick.placeholder"))} aria-label={kind === "goal" ? t("todos:quick.goalLabel") : t("todos:quick.label")}
          className="min-w-0 flex-1 bg-transparent type-body text-foreground outline-none placeholder:text-tertiary" data-quick-input />
        <span className="hidden shrink-0 type-footnote text-tertiary sm:inline">{t("todos:quick.enter")}</span>
      </form>
      {tokens.length > 0 && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5 px-1" data-quick-chips>
          {tokens.map((x) => (
            <span key={x.key} className="inline-flex h-6 items-center gap-1 rounded-md bg-primary/10 pl-2 pr-1 type-footnote text-primary" data-quick-chip={x.kind}>
              {chip(x)}
              <button type="button" onClick={() => remove(x)} aria-label={t("todos:quick.remove", { what: x.raw.trim() })} className="flex size-4 items-center justify-center rounded-sm hover:bg-primary/15"><XIcon className="size-3" /></button>
            </span>
          ))}
          <span className="type-footnote text-tertiary">{t("todos:quick.rules")}</span>
        </div>
      )}
    </div>
  )
}

/* ───────── actions on a todo ───────── */
/** hands a todo to a Bot: a new item from its words (the Bot is chosen as for any new item), and the todo marked handed
 *  to it. The item is not taken back by undo: it is shown in the toast instead. */
export async function handToBot(x: Todo) {
  const id = await newItem(x.text)
  if (!id) return
  const r = await change(todosApi.state(x.id, "handed", id))
  if (r) toast(t_("todos:toast.handed", { id }), { action: { label: t_("todos:action.openItem"), onClick: () => go("#/items/" + id) } })
}
export const complete = (x: Todo, done: boolean) =>
  change(todosApi.state(x.id, done ? "done" : "open"), done ? t_(x.recur ? "todos:toast.doneNext" : "todos:toast.done", { text: x.text }) : t_("todos:toast.reopened"))
export const drop = (x: Todo) => change(todosApi.state(x.id, "dropped"), t_("todos:toast.dropped", { text: x.text }))
export const snooze = (x: Todo, until: number | null) => change(todosApi.patch(x.id, { snooze_until: until }), until ? t_("todos:toast.snoozed") : t_("todos:toast.unsnoozed"))
export const setStar = (x: Todo, star: boolean) => change(todosApi.patch(x.id, { star }))

/** 推迟: this evening, tomorrow morning or next Monday, and back when it is snoozed */
export function SnoozeMenu({ x, now, children }: { x: Todo; now: number; children?: ReactNode }) {
  const t = useT(), due = useDueText()
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button size="sm" variant="ghost" className="h-7 gap-1 px-2 text-muted-foreground" data-todo-snooze />}>{children ?? <>{t("todos:action.snooze")}<ChevronDownIcon className="size-3.5" /></>}</DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-44">
        {snoozeChoices(now).map((c) => <DropdownMenuItem key={c.key} onClick={() => void snooze(x, c.at)} data-snooze={c.key}>{t(`todos:snooze.${c.key}`)}<span className="ml-auto pl-4 type-footnote text-muted-foreground">{due(at(c.at), now)}</span></DropdownMenuItem>)}
        {isSnoozed(x, now) && <><DropdownMenuSeparator /><DropdownMenuItem onClick={() => void snooze(x, null)} data-snooze="clear">{t("todos:snooze.clear")}</DropdownMenuItem></>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/** the rest of a todo's actions, behind "…" */
export function TodoMenu({ x, now }: { x: Todo; now: number }) {
  const t = useT()
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button size="icon-sm" variant="ghost" className="size-7 text-muted-foreground" aria-label={t("todos:action.more", { text: x.text })} data-todo-more />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-44">
        <DropdownMenuItem onClick={() => go("#/todos/" + x.id)}>{t("todos:action.open")}</DropdownMenuItem>
        {x.state === "open" && <DropdownMenuItem onClick={() => void handToBot(x)} data-menu-hand><BotIcon />{t("todos:action.hand")}</DropdownMenuItem>}
        {x.state === "open" && snoozeChoices(now).map((c) => <DropdownMenuItem key={c.key} onClick={() => void snooze(x, c.at)}><CalendarClockIcon />{t(`todos:snooze.${c.key}`)}</DropdownMenuItem>)}
        <DropdownMenuItem onClick={() => void setStar(x, !x.star)} data-menu-star><StarIcon />{x.star ? t("todos:action.unstar") : t("todos:action.star")}</DropdownMenuItem>
        <DropdownMenuSeparator />
        {x.state === "open"
          ? <DropdownMenuItem onClick={() => void drop(x)} data-menu-drop><CircleSlashIcon />{t("todos:action.drop")}</DropdownMenuItem>
          : x.state !== "handed" && <DropdownMenuItem onClick={() => void complete(x, false)} data-menu-reopen><UndoIcon />{t("todos:action.reopen")}</DropdownMenuItem>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/* ───────── a todo's row ───────── */
/** where a todo came from, when Steve or an agent wrote it down: "Steve 记的 · 来自 邮件", the source's words on hover */
function Origin({ x }: { x: Todo }) {
  const t = useT()
  if (x.source === "you") return null
  const s = x.sources[0], label = x.source === "steve" ? t("todos:origin.steve") : t("todos:origin.agent")
  return (
    <span className="relative z-[1] inline-flex h-5 min-w-0 items-center gap-1 rounded-md bg-muted/70 px-1.5 type-label text-muted-foreground" title={x.due_quote ?? s?.excerpt ?? undefined} data-todo-origin={x.source}>
      <SparklesIcon className="size-3 shrink-0" aria-hidden /><span className="truncate">{s?.name ? t("todos:origin.from", { who: label, from: s.name }) : label}</span>
    </span>
  )
}
/** a link to a goal, as a chip */
export function GoalChip({ goal }: { goal: { id: string; text: string | null } }) {
  return <a href={"#/todos/" + goal.id} className="relative z-[1] inline-flex h-5 min-w-0 max-w-56 items-center gap-1 rounded-md bg-primary/10 px-1.5 type-label text-primary hover:bg-primary/15" data-todo-goal={goal.id}>
    <TargetIcon className="size-3 shrink-0" aria-hidden /><span className="truncate">{goal.text ?? goal.id}</span></a>
}

/** One todo: a box to tick, its words (they open it), what it belongs to and where it came from, and on the right its
 *  deadline. Hovering (or, on a phone, "…") offers 交给 Bot and 推迟. A todo handed to a Bot shows its item instead. */
export function TodoRow({ x, now, showGoal = true }: { x: Todo; now: number; showGoal?: boolean }) {
  const t = useT(), due = useDueText()
  const w = dueWhen(x, now), item = x.item_id ? st.items.find((i) => i.id === x.item_id) : undefined
  if (x.state === "handed") return (
    <div role="listitem" className="group/todo relative flex min-h-12 items-center gap-3 rounded-lg px-3 py-2 hover:bg-accent" data-todo={x.id} data-todo-state="handed">
      <a href={"#/items/" + (x.item_id ?? "")} className="absolute inset-0 rounded-[inherit]" aria-label={t("todos:row.openItem", { id: x.item_id })} />
      <span className="flex size-5 shrink-0 items-center justify-center rounded-md bg-primary/12 text-primary" aria-hidden><BotIcon className="size-3.5" /></span>
      <div className="min-w-0 flex-1">
        <div className="truncate type-body">{x.text}</div>
        <div className="truncate type-footnote text-muted-foreground">{t("todos:row.handed", { id: x.item_id, title: item?.t ?? "" })}</div>
      </div>
      {item ? <Status s={item.st} /> : <span className="type-footnote text-muted-foreground">{x.item_id}</span>}
      {/* where the others have "…", so the right edges line up */}
      <span className="w-7 shrink-0" aria-hidden />
    </div>
  )
  const closed = x.state === "done" || x.state === "dropped"
  return (
    <div role="listitem" className="group/todo relative flex min-h-12 items-start gap-3 rounded-lg px-3 py-2.5 transition-colors duration-120 hover:bg-accent focus-within:bg-accent" data-todo={x.id} data-todo-state={x.state}>
      <span className="relative z-[1] pt-0.5">
        {x.state === "dropped"
          ? <span className="flex size-4 items-center justify-center text-tertiary" title={t("todos:row.dropped")}><CircleSlashIcon className="size-4" /></span>
          : <Checkbox checked={x.state === "done"} onCheckedChange={(v) => void complete(x, v)} aria-label={t(x.state === "done" ? "todos:row.undone" : "todos:row.done", { text: x.text })} data-todo-check />}
      </span>
      <div className="min-w-0 flex-1">
        <a href={"#/todos/" + x.id} className={cn("block type-body outline-none after:absolute after:inset-0 after:rounded-[inherit]", closed && "text-muted-foreground line-through decoration-tertiary")} data-todo-open>
          {x.star && <StarIcon className="-mt-0.5 mr-1 inline size-3.5 fill-current text-attention" aria-label={t("todos:row.starred")} />}{x.text}
        </a>
        {(x.parent && showGoal) || x.matter || x.source !== "you" || x.people.length || x.recur || x.pri ? (
          <div className="mt-1 flex min-w-0 flex-wrap items-center gap-1.5">
            {x.parent && showGoal && <GoalChip goal={x.parent} />}
            {x.matter && <span className="inline-flex h-5 max-w-56 items-center truncate rounded-md bg-muted/70 px-1.5 type-label text-muted-foreground" data-todo-matter>{x.matter.title ?? t("todos:row.matter")}</span>}
            <Origin x={x} />
            {x.people.filter((p) => p.name).map((p) => <span key={p.id} className="type-label text-muted-foreground">{p.name}</span>)}
            {x.recur && <span className="inline-flex items-center gap-1 type-label text-muted-foreground" data-todo-recur><RepeatIcon className="size-3" aria-hidden />{t(`todos:recur.${recurKey(x.recur)}`)}</span>}
            {x.pri > 0 && <span className="type-label text-muted-foreground" data-todo-pri={x.pri}>{t("todos:quick.pri", { pri: t(`todos:pri.${x.pri as 1 | 2 | 3}`) })}</span>}
          </div>
        ) : null}
      </div>
      {/* the actions come in over the deadline on hover; on a phone "…" holds them */}
      {x.state === "open" && <div className="relative z-[1] hidden shrink-0 items-center gap-1 opacity-0 transition-opacity group-focus-within/todo:opacity-100 group-hover/todo:opacity-100 md:flex" data-todo-actions>
        <Button size="sm" variant="outline" className="h-7 gap-1 px-2" onClick={() => void handToBot(x)} data-todo-hand><BotIcon className="size-3.5" />{t("todos:action.hand")}</Button>
        <SnoozeMenu x={x} now={now} />
      </div>}
      <span className={cn("shrink-0 pt-0.5 type-footnote tabular-nums md:group-hover/todo:hidden md:group-focus-within/todo:hidden", w?.overdue && !closed ? "text-destructive" : "text-muted-foreground")} data-todo-due>
        {isSnoozed(x, now) && x.state === "open" ? t("todos:row.snoozedUntil", { when: due(at(x.snooze_until!), now) }) : x.state === "dropped" ? t("todos:row.dropped") : x.state === "done" && x.closed ? due(at(x.closed), now) : w ? (w.time && w.rel === "today" && !w.overdue ? t("todos:row.before", { time: w.time }) : due(x, now)) : null}
      </span>
      <span className="relative z-[1] md:hidden"><TodoMenu x={x} now={now} /></span>
      <span className="relative z-[1] hidden md:inline-flex md:opacity-0 md:group-hover/todo:opacity-100 md:group-focus-within/todo:opacity-100"><TodoMenu x={x} now={now} /></span>
    </div>
  )
}
/** FREQ=WEEKLY;INTERVAL=2 → its words' key; an interval other than 1 is said as "every N" */
export const recurKey = (r: string) => {
  const f = /FREQ=(DAILY|WEEKLY|MONTHLY|YEARLY)/.exec(r)?.[1]?.toLowerCase() ?? "custom", n = /INTERVAL=(\d+)/.exec(r)?.[1]
  return (n && n !== "1" ? "custom" : f) as "daily" | "weekly" | "monthly" | "yearly" | "custom"
}

/* ───────── goals ───────── */
/** a goal's progress: the percentage large, done out of what counts, and the bar; "还没拆" when nothing counts */
export function GoalProgress({ g, size = "md" }: { g: Todo; size?: "md" | "lg" }) {
  const t = useT(), p = g.progress
  if (!p || p.pct == null) return (
    <div className="flex items-baseline gap-2" data-goal-progress="none">
      <span className={cn("font-semibold tracking-tight text-tertiary", size === "lg" ? "text-[40px] leading-none" : "text-[28px] leading-none")}>—</span>
      <span className="type-footnote text-muted-foreground">{p && p.total ? t("todos:goal.allDropped") : t("todos:goal.notSplit")}</span>
    </div>
  )
  return (
    <div className="flex flex-col gap-2" data-goal-progress={p.pct}>
      <div className="flex items-baseline gap-2">
        <span className={cn("font-semibold tabular-nums tracking-tight", size === "lg" ? "text-[40px] leading-none" : "text-[28px] leading-none")}>{p.pct}%</span>
        <span className="type-footnote tabular-nums text-muted-foreground">{t("todos:goal.counts", { done: p.done, total: p.total - p.dropped })}{p.dropped ? " · " + t("todos:goal.dropped", { count: p.dropped }) : ""}</span>
      </div>
      <Progress value={p.pct} aria-label={t("todos:goal.progressLabel", { pct: p.pct })} className="gap-0 [&_[data-slot=progress-track]]:h-1.5" />
    </div>
  )
}

/** a goal on the todo page: its name, target date, progress; it opens the goal's page */
export function GoalCard({ g, now }: { g: Todo; now: number }) {
  const t = useT(), due = useDueText()
  const left = daysLeft(g, now)
  return (
    <Card className="relative flex flex-col gap-3 transition-shadow duration-120 hover:shadow-[var(--lift-hover)]" data-goal-card={g.id}>
      <div className="flex items-start gap-2">
        <TargetIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
        <a href={"#/todos/" + g.id} className={cn("min-w-0 flex-1 type-headline outline-none after:absolute after:inset-0 after:rounded-[inherit]", g.state !== "open" && "text-muted-foreground line-through")}>{g.text}</a>
        {g.target_date && <span className={cn("shrink-0 type-footnote tabular-nums", left != null && left < 0 && g.state === "open" ? "text-destructive" : "text-muted-foreground")} title={left != null ? t("todos:goal.daysLeft", { count: left }) : undefined}>{due({ due: null, due_date: g.target_date }, now)}</span>}
      </div>
      <GoalProgress g={g} />
    </Card>
  )
}
/** days from today to a goal's target date (negative once past) */
export const daysLeft = (g: Pick<Goal, "target_date">, now: number) => g.target_date ? Math.round((dateOf(g.target_date).getTime() - dateOf(dayOf(now)).getTime()) / 86_400_000) : null
