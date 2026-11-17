/* 待办: what you yourself are to do, and your goals. Not 待处理 (cards waiting for your decision) and not 事项 (work a
 * Bot does): a todo handed to a Bot becomes an item and stays here, showing it. Every member sees only their own.
 *
 *   #/todos              今天: overdue, today, this week, no date, lately handed; goals beside it
 *   #/todos?view=all     全部: also later, snoozed, everything handed and the latest closed
 *   #/todos?view=goals   目标: every goal as a card
 *   #/todos/<id>         a todo's details, or a goal's page */
import { useEffect, useMemo, useState, type ReactNode } from "react"
import { BotIcon, CheckIcon, CircleSlashIcon, LinkIcon, TargetIcon, UnlinkIcon, UndoIcon } from "lucide-react"
import { cn } from "cn"
import { HubError } from "@/api"
import { Button } from "@/components/ui/button"
import { DateField, TimeField } from "@/components/ui/pickers"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Switch } from "@/components/ui/switch"
import { useT } from "@/i18n"
import { st, useHubEvents, useStore } from "@/store"
import { Blank, Card, Empty, go, Loading, Note, Panel, Row, Section, Segmented, SearchField, Status, Page } from "@/ui"
import { Count, Select } from "@/view-shared"
import { change, isTodoEvent, loadTodos, todosApi, useMinute, useTodos } from "./data"
import { takeQuickAdd } from "./quick"
import { dayOf, groups, RECURS, type Bucket, type Goal, type Todo } from "./model"
import { complete, daysLeft, drop, GoalCard, GoalProgress, handing, handToBot, QuickAdd, recurKey, setStar, SnoozeMenu, TodoMenu, TodoRow, useDueText } from "./parts"

type Tab = "today" | "all" | "goals"
const TABS: Tab[] = ["today", "all", "goals"]

function Tabs({ tab }: { tab: Tab }) {
  const t = useT()
  return <Segmented value={tab} onChange={(v) => go(v === "today" ? "#/todos" : "#/todos?view=" + v)} label={t("todos:tabs.label")} options={TABS.map((x) => [x, t(`todos:tabs.${x}`)] as const)} />
}

/** the page's state while the list loads or fails */
function useListState() {
  const t = useT(), s = useTodos()
  if (s.todos) return { todos: s.todos, gate: null }
  if (s.error) return { todos: null, gate: <Empty title={s.unwired ? t("todos:list.unwired") : t("todos:list.failed")} sub={s.unwired ? t("todos:list.unwiredSub") : s.error} action={<Button variant="outline" onClick={() => void loadTodos()} data-todos-retry>{t("common:action.retry")}</Button>} data-todos-error /> }
  return { todos: null, gate: <Loading /> }
}

export function TodosView({ query }: { query?: string }) {
  useStore()
  const t = useT(), now = useMinute()
  const view = new URLSearchParams(query ?? "").get("view")
  const tab: Tab = view === "all" || view === "goals" ? view : "today"
  const { todos, gate } = useListState()
  const [autoFocus] = useState(takeQuickAdd)
  const goals = useMemo(() => (todos ?? []).filter((x) => x.kind === "goal"), [todos])
  const open = goals.filter((g) => g.state === "open")
  const gs = todos ? groups(todos, tab === "all" ? "all" : "today", now) : []
  const goalColumn = (
    <aside className="flex min-w-0 flex-col gap-3" data-todos-goals>
      <div className="flex items-center justify-between px-1"><span className="type-label text-tertiary">{t("todos:goals.head")}<Count n={open.length} /></span>
        {open.length > 0 && <a href="#/todos?view=goals" className="type-footnote text-muted-foreground hover:text-foreground">{t("todos:goals.all")}</a>}</div>
      {open.map((g) => <GoalCard key={g.id} g={g} now={now} />)}
      <QuickAdd goals={goals} kind="goal" />
      <Note className="mt-0">{t("todos:goals.note")}</Note>
    </aside>
  )
  return (
    <Page title={t("todos:title")} sub={t("todos:sub")} max={tab === "goals" ? 760 : 1080} toolbar={<Tabs tab={tab} />}>
      <div data-todos={tab}>
        {gate ?? (tab === "goals" ? <GoalsTab todos={todos!} now={now} /> : (
          <div className="grid min-w-0 gap-8 lg:grid-cols-[minmax(0,1fr)_300px]">
            <div className="flex min-w-0 flex-col gap-6">
              <QuickAdd goals={goals} autoFocus={autoFocus} />
              {gs.length ? gs.map(([b, xs]) => <TodoGroup key={b} b={b} xs={xs} now={now} recent={tab === "today"} />)
                : <Empty compact icon={<CheckIcon />} title={tab === "today" ? t("todos:list.clearToday") : t("todos:list.none")} sub={t("todos:list.noneSub")} data-todos-empty />}
            </div>
            {goalColumn}
          </div>
        ))}
      </div>
    </Page>
  )
}

/** one group of the list: its label and count (逾期 in red), then its rows */
function TodoGroup({ b, xs, now, recent }: { b: Bucket; xs: Todo[]; now: number; recent: boolean }) {
  const t = useT()
  return (
    <section className="min-w-0" data-todo-group={b}>
      <div className={cn("mb-1 px-1 type-label", b === "overdue" ? "text-destructive" : "text-tertiary")}>{t(b === "handed" && recent ? "todos:group.handedRecent" : `todos:group.${b}`)}<Count n={xs.length} /></div>
      <div role="list" className="-mx-3 flex flex-col">{xs.map((x) => <TodoRow key={x.id} x={x} now={now} />)}</div>
    </section>
  )
}

function GoalsTab({ todos, now }: { todos: Todo[]; now: number }) {
  const t = useT()
  const goals = todos.filter((x) => x.kind === "goal"), open = goals.filter((g) => g.state === "open"), closed = goals.filter((g) => g.state !== "open")
  return (
    <div className="flex flex-col gap-8">
      <QuickAdd goals={goals} kind="goal" />
      {open.length ? <div className="grid gap-3 sm:grid-cols-2">{open.map((g) => <GoalCard key={g.id} g={g} now={now} />)}</div>
        : <Empty compact icon={<TargetIcon />} title={t("todos:goals.none")} sub={t("todos:goals.noneSub")} data-goals-empty />}
      {closed.length > 0 && <Section label={<>{t("todos:goals.closed")}<Count n={closed.length} /></>}><div className="grid gap-3 sm:grid-cols-2">{closed.map((g) => <GoalCard key={g.id} g={g} now={now} />)}</div></Section>}
      <Explain />
    </div>
  )
}
/** what a goal is next to 事 (a matter) and 项目 (a project) */
function Explain() {
  const t = useT()
  return <Section label={t("todos:explain.head")}><Card className="flex flex-col gap-1.5 type-footnote text-muted-foreground" data-goal-explain>
    <p>{t("todos:explain.goal")}</p><p>{t("todos:explain.matter")}</p><p>{t("todos:explain.project")}</p></Card></Section>
}

/* ───────── one todo or goal ───────── */
export function TodoRoute({ id }: { id: string }) {
  useStore()
  const t = useT()
  const [x, setX] = useState<Todo | Goal | null>(null), [err, setErr] = useState<{ msg: string; missing: boolean } | null>(null)
  const load = () => todosApi.get(id).then((r) => { setX(r); setErr(null) }, (e) => setErr({ msg: (e as Error).message, missing: e instanceof HubError && e.status === 404 }))
  useEffect(() => { void load() }, [id]) // eslint-disable-line react-hooks/exhaustive-deps
  useHubEvents(["digest"], () => void load(), 300, { filter: isTodoEvent })
  const back: [string, string] = ["#/todos", t("todos:title")]
  if (err) return <Page title={t("todos:detail.title")} back={back}><Empty title={err.missing ? t("todos:detail.missing") : t("todos:list.failed")} sub={err.missing ? t("todos:detail.missingSub") : err.msg} data-todo-missing /></Page>
  if (!x) return <Page title={t("todos:detail.title")} back={back}><Loading /></Page>
  return x.kind === "goal" ? <GoalPage g={x as Goal} reload={load} /> : <TodoDetail x={x} />
}

/** a title that becomes a field on click */
function EditableTitle({ text, onSave, done }: { text: string; onSave: (v: string) => void; done?: boolean }) {
  const t = useT(), [edit, setEdit] = useState(false)
  if (edit) return <Input autoFocus defaultValue={text} aria-label={t("todos:detail.rename")} className="h-9 rounded-lg type-title" data-todo-title-input
    onBlur={(e) => { setEdit(false); const v = e.target.value.trim(); if (v && v !== text) onSave(v) }}
    onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); if (e.key === "Escape") setEdit(false) }} />
  return <button type="button" className={cn("text-left hover:text-foreground/80", done && "text-muted-foreground line-through")} onClick={() => setEdit(true)} title={t("todos:detail.rename")} data-todo-title>{text}</button>
}

/** a labelled line of the detail panel */
function Line({ label, children, k }: { label: string; children: ReactNode; k: string }) {
  return <div className="flex min-h-12 flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2" data-todo-field={k}><span className="w-24 shrink-0 type-callout text-muted-foreground">{label}</span><div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">{children}</div></div>
}

function TodoDetail({ x }: { x: Todo }) {
  const t = useT(), now = useMinute(), due = useDueText(), s = useTodos()
  const goals = (s.todos ?? []).filter((g) => g.kind === "goal" && (g.state === "open" || g.id === x.parent?.id))
  const save = (p: Parameters<typeof todosApi.patch>[1], done = t("common:state.saved")) => change(todosApi.patch(x.id, p), done)
  const time = x.due != null ? new Date(x.due) : null, day = x.due_date ?? (time ? dayOf(time) : null)
  const hm = time ? `${String(time.getHours()).padStart(2, "0")}:${String(time.getMinutes()).padStart(2, "0")}` : null
  const setDue = (d: string | null, h: string | null) => {
    if (!d) return save({ due: null, due_date: null })
    if (!h) return save({ due_date: d })
    const at = new Date(d + "T00:00:00"); const [hh, mm] = h.split(":").map(Number); at.setHours(hh, mm, 0, 0)
    return save({ due: at.getTime() })
  }
  const open = x.state === "open"
  return (
    <Page small title={<EditableTitle text={x.text} done={!open} onSave={(text) => void save({ text })} />} back={["#/todos", t("todos:title")]} max={760}
      actions={<TodoMenu x={x} now={now} />}
      toolbar={open ? <>
        <Button size="sm" onClick={() => void complete(x, true)} data-detail-done><CheckIcon />{t("todos:action.done")}</Button>
        <Button size="sm" variant="outline" disabled={handing(x)} onClick={() => void handToBot(x)} data-detail-hand><BotIcon />{t("todos:action.hand")}</Button>
        <SnoozeMenu x={x} now={now} />
      </> : x.state === "handed" ? <Button size="sm" variant="outline" onClick={() => go("#/items/" + x.item_id)}><BotIcon />{t("todos:action.openItem")}</Button>
        : <Button size="sm" variant="outline" onClick={() => void complete(x, false)} data-detail-reopen><UndoIcon />{t("todos:action.reopen")}</Button>}>
      <div className="flex flex-col gap-8" data-todo-detail={x.id}>
        {x.state !== "open" && <Blank data-todo-closed>{x.state === "done" ? t("todos:detail.isDone", { when: x.closed ? due({ due: x.closed, due_date: null }, now) : "" }) : x.state === "dropped" ? t("todos:detail.isDropped") : t("todos:detail.isHanded", { id: x.item_id })}</Blank>}
        <Card className="p-1">
          <Line label={t("todos:field.due")} k="due">
            <DateField value={day} onChange={(d) => void setDue(d, hm)} size="sm" className="w-48" aria-label={t("todos:field.dueDay")} />
            <TimeField value={hm} onChange={(h) => day && void setDue(day, h)} size="sm" className="w-28" aria-label={t("todos:field.dueTime")} />
            {day && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => void setDue(null, null)} data-due-clear>{t("todos:field.clear")}</Button>}
            {x.due_quote && <span className="w-full type-footnote text-muted-foreground" data-due-quote>{t("todos:field.quote", { quote: x.due_quote })}</span>}
          </Line>
          <Line label={t("todos:field.recur")} k="recur">
            <Select value={x.recur ?? "none"} disabled={!day} onChange={(v) => void save({ recur: v === "none" ? null : v })} label={t("todos:field.recur")} testId="todo-recur"
              options={[{ v: "none", l: t("todos:recur.none") }, ...RECURS.map((r) => ({ v: r, l: t(`todos:recur.${recurKey(r)}`) })), ...(x.recur && !(RECURS as readonly string[]).includes(x.recur) ? [{ v: x.recur, l: t("todos:recur.custom") }] : [])]} />
            {!day && <span className="type-footnote text-muted-foreground">{t("todos:field.recurNeedsDue")}</span>}
          </Line>
          <Line label={t("todos:field.goal")} k="goal">
            <Select value={x.parent?.id ?? "none"} onChange={(v) => void save({ parent: v === "none" ? null : v })} label={t("todos:field.goal")} testId="todo-goal"
              options={[{ v: "none", l: t("todos:field.noGoal") }, ...goals.map((g) => ({ v: g.id, l: g.text }))]} />
          </Line>
          <Line label={t("todos:field.pri")} k="pri">
            <Segmented value={String(x.pri)} onChange={(v) => void save({ pri: Number(v) })} size="sm" pressed label={t("todos:field.pri")} options={(["0", "1", "2", "3"] as const).map((p) => [p, p === "0" ? t("todos:pri.0") : t(`todos:pri.${p}`)] as const)} />
          </Line>
          <Line label={t("todos:field.star")} k="star">
            <Switch checked={x.star} onCheckedChange={(v: boolean) => void setStar(x, v)} aria-label={t("todos:field.star")} />
          </Line>
        </Card>
        <Section label={t("todos:field.origin")}>
          <Panel>
            <Row title={x.source === "you" ? t("todos:origin.you") : x.source === "steve" ? t("todos:origin.steve") : t("todos:origin.agent")} sub={t("todos:detail.created", { when: due({ due: x.created, due_date: null }, now) })} />
            {x.matter && <Row title={x.matter.title ?? t("todos:row.matter")} sub={t("todos:detail.matter")} />}
            {x.sources.map((s) => <Row key={s.conv + s.message} title={s.name ?? s.conv} sub={s.excerpt ?? undefined} onClick={s.href ? () => { location.hash = s.href! } : undefined} />)}
          </Panel>
        </Section>
      </div>
    </Page>
  )
}

/* ───────── a goal's page ───────── */
function GoalPage({ g, reload }: { g: Goal; reload: () => void }) {
  const t = useT(), now = useMinute(), due = useDueText()
  const p = g.progress, left = daysLeft(g, now)
  const items = g.items.map((id) => ({ id, i: st.items.find((x) => x.id === id) }))
  const mine = g.children.filter((c) => c.state !== "handed").length, bots = g.children.filter((c) => c.state === "handed").length + items.length
  const running = items.filter((x) => x.i && x.i.st !== "done" && x.i.st !== "drop").length + g.children.filter((c) => c.state === "handed" && st.items.find((i) => i.id === c.item_id && i.st !== "done" && i.st !== "drop")).length
  const save = (patch: Parameters<typeof todosApi.patch>[1]) => change(todosApi.patch(g.id, patch), t("common:state.saved"))
  const open = g.state === "open"
  const children = [...g.children].sort((a, b) => Number(a.state !== "open") - Number(b.state !== "open"))
  return (
    <Page small max={1080} title={<EditableTitle text={g.text} done={!open} onSave={(text) => void save({ text })} />} back={["#/todos?view=goals", t("todos:title")]}
      toolbar={open ? <>
        <Button size="sm" variant="outline" onClick={() => void complete(g, true).then(reload)} data-goal-done><CheckIcon />{t("todos:goal.done")}</Button>
        <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => void drop(g).then(reload)} data-goal-drop><CircleSlashIcon />{t("todos:goal.drop")}</Button>
      </> : <Button size="sm" variant="outline" onClick={() => void complete(g, false).then(reload)} data-goal-reopen><UndoIcon />{t("todos:action.reopen")}</Button>}>
      <div className="grid min-w-0 gap-8 lg:grid-cols-[minmax(0,1fr)_300px]" data-goal-page={g.id}>
        <div className="flex min-w-0 flex-col gap-6">
          <div className="flex flex-wrap items-center gap-2 type-footnote text-muted-foreground" data-goal-meta>
            <TargetIcon className="size-3.5" aria-hidden />
            <span className="inline-flex items-center gap-2">{t("todos:goal.target")}
              <DateField value={g.target_date} onChange={(d) => void save({ target_date: d })} size="sm" className="w-48" aria-label={t("todos:goal.target")} /></span>
            {g.target_date && <Button size="sm" variant="ghost" className="h-7 px-2 text-muted-foreground" onClick={() => void save({ target_date: null })}>{t("todos:field.clear")}</Button>}
            {left != null && open && <span className={left < 0 ? "text-destructive" : undefined} data-goal-left>{left < 0 ? t("todos:goal.overdueBy", { count: -left }) : t("todos:goal.daysLeft", { count: left })}</span>}
            {g.matter?.title && <span className="rounded-md bg-muted/70 px-1.5 py-0.5 type-label">{t("todos:goal.matterChip", { title: g.matter.title })}</span>}
          </div>
          <Card className="flex flex-col gap-2" data-goal-progress-card>
            <GoalProgress g={g} size="lg" />
            {p && p.pct != null && running > 0 && <span className="type-footnote text-muted-foreground">{t("todos:goal.running", { count: running })}</span>}
            <span className="type-footnote text-tertiary">{t("todos:goal.rule")}</span>
          </Card>
          <section className="min-w-0" data-goal-split>
            <div className="mb-1 flex flex-wrap items-center justify-between gap-2 px-1">
              <span className="type-label text-tertiary">{t("todos:goal.split", { count: g.children.length + items.length })}{g.children.length + items.length ? " · " + t("todos:goal.splitWho", { mine, bots }) : ""}</span>
              {open && <LinkItem g={g} onDone={reload} />}
            </div>
            <div role="list" className="-mx-3 flex flex-col">
              {children.map((c) => <TodoRow key={c.id} x={c} now={now} showGoal={false} />)}
              {items.map(({ id, i }) => (
                <div role="listitem" key={id} className="group/link relative flex min-h-12 items-center gap-3 rounded-lg px-3 py-2 hover:bg-accent" data-goal-item={id}>
                  <a href={"#/items/" + id} className="absolute inset-0 rounded-[inherit]" aria-label={t("todos:row.openItem", { id })} />
                  <span className="flex size-5 shrink-0 items-center justify-center rounded-md bg-primary/12 text-primary" aria-hidden><BotIcon className="size-3.5" /></span>
                  <div className="min-w-0 flex-1"><div className={cn("truncate type-body", i?.st === "done" && "text-muted-foreground line-through")}>{i?.t ?? id}</div>
                    <div className="type-footnote text-muted-foreground">{t("todos:goal.itemLine", { id })}</div></div>
                  {i && <Status s={i.st} />}
                  {open && <Button size="icon-sm" variant="ghost" className="relative z-[1] size-7 text-muted-foreground opacity-100 md:opacity-0 md:group-hover/link:opacity-100" aria-label={t("todos:goal.unlink", { id })} onClick={() => void change(todosApi.link(g.id, id, false), t("todos:toast.unlinked", { id })).then(reload)} data-goal-unlink><UnlinkIcon /></Button>}
                </div>
              ))}
            </div>
            {!g.children.length && !items.length && <Blank>{t("todos:goal.empty")}</Blank>}
            {open && <QuickAdd goals={[]} parent={g.id} className="mt-3" placeholder={t("todos:quick.childPlaceholder")} />}
          </section>
        </div>
        <aside className="flex min-w-0 flex-col gap-6">
          {g.target_date && <Section label={t("todos:goal.when")}><Card className="type-callout" data-goal-when>{t("todos:goal.targetOn", { day: due({ due: null, due_date: g.target_date }, now) })}<Note className="mt-1 px-0">{t("todos:goal.calendarLater")}</Note></Card></Section>}
          <Explain />
        </aside>
      </div>
    </Page>
  )
}

/** links one of the workspace's items to the goal: it then counts in the progress */
function LinkItem({ g, onDone }: { g: Goal; onDone: () => void }) {
  const t = useT(), [open, setOpen] = useState(false), [q, setQ] = useState("")
  const taken = new Set([...g.items, ...g.children.map((c) => c.item_id).filter(Boolean) as string[]])
  const x = q.trim().toLowerCase()
  const list = st.items.filter((i) => !taken.has(i.id) && !i.arch && (!x || `${i.id} ${i.t}`.toLowerCase().includes(x))).slice(0, 8)
  return (
    <Popover open={open} onOpenChange={(o) => { setOpen(o); if (!o) setQ("") }}>
      <PopoverTrigger render={<Button size="sm" variant="ghost" className="h-7 gap-1 px-2 text-muted-foreground" data-goal-link />}><LinkIcon className="size-3.5" />{t("todos:goal.link")}</PopoverTrigger>
      <PopoverContent align="end" className="w-80 gap-2 p-2">
        <SearchField size="sm" autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("todos:goal.linkSearch")} aria-label={t("todos:goal.linkSearch")} />
        <div className="flex max-h-72 flex-col overflow-y-auto" role="listbox" aria-label={t("todos:goal.link")}>
          {list.length ? list.map((i) => (
            <button key={i.id} type="button" role="option" aria-selected={false} className="flex min-h-9 items-center gap-2 rounded-md px-2 text-left hover:bg-accent" data-link-option={i.id}
              onClick={() => { setOpen(false); void change(todosApi.link(g.id, i.id), t("todos:toast.linked", { id: i.id })).then(onDone) }}>
              <span className="shrink-0 font-mono type-footnote text-muted-foreground">{i.id}</span><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span><Status s={i.st} />
            </button>
          )) : <div className="px-2 py-3 type-footnote text-muted-foreground">{t("todos:goal.linkNone")}</div>}
        </div>
      </PopoverContent>
    </Popover>
  )
}

