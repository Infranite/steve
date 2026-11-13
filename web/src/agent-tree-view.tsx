/* ── the item page's collaboration area: an agent tree ──
 * One card per agent: who it is (Bot and model), what it was asked to do, its state, and what it cost and took.
 * The coordinator's direct children sit on a bus under it while they fit side by side; anything deeper, and
 * everything on a narrow page, hangs from a rail under the card that started it. The header line counts the
 * agents, the running ones, failures (a pill opening them with retry) and cards waiting for you, and the total
 * cost. A card selected shows its detail in the page's side column (a sheet on narrow pages). */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { CheckIcon, ChevronDownIcon, ChevronRightIcon, CircleCheckIcon, CircleDashedIcon, CirclePauseIcon, CircleStopIcon, GitForkIcon, HourglassIcon, LoaderCircleIcon, MessageCircleQuestionIcon, MoreHorizontalIcon, NetworkIcon, PauseIcon, PlayIcon, RotateCcwIcon, SquareIcon, TriangleAlertIcon, XIcon } from "lucide-react"
import { api } from "@/api"
import { stats as statsApi } from "@/api/stats"
import { Button } from "@/components/ui/button"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet"
import { useT } from "@/i18n"
import { clockDur } from "@/i18n/format"
import { cn } from "@/lib/utils"
import { fmtN, fmtUsd } from "@/stats/format"
import { botName, loadItem, st } from "@/store"
import { BotAvatar } from "@/ui"
import { orderedUnits, type Workflow } from "@/workflow"
import { buildAgentTree, workTime, findNode, isActive, isOver, rollup, summarize, type AgentNode, type AgentStatus, type Cost, type TreeItem, type TreeStat } from "@/agent-tree"

type Action = "pause" | "resume" | "retry" | "cancel" | "single_session"

/** the tree of `root`, its figures, and the work graph's actions (one revision-checked change at a time) */
export function useAgentTree(root: string, workflow?: Workflow | null, rootStatus?: AgentStatus) {
  const t = useT()
  const tree = buildAgentTree(root, st.items as TreeItem[], workflow, 8, rootStatus)
  const summary = tree ? summarize(tree) : null
  // the figures are kept with the root they belong to: another item's never show while this one's load
  const [loaded, setLoaded] = useState<{ root: string; nodes: TreeStat[] }>({ root: "", nodes: [] })
  const rows = loaded.root === root ? loaded.nodes : []
  const shown = !!tree && (tree.children.length > 0 || !!workflow)
  const live = !!summary?.active
  // what changes the figures: a node added, or one changing state; while something runs, a slow refresh too
  const shape = tree ? JSON.stringify(collect(tree).map((n) => [n.key, n.status])) : ""
  useEffect(() => {
    if (!shown) return
    let on = true
    const load = () => statsApi.tree(root).then((r) => { if (on) setLoaded({ root, nodes: r.nodes }) }).catch(() => {})
    void load()
    const timer = live ? setInterval(load, 15_000) : undefined
    return () => { on = false; clearInterval(timer) }
  }, [root, shown, live, shape])
  const byItem = new Map(rows.map((s) => [s.item_id, s]))
  const totals = rollup(rows)

  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  const [rework, setRework] = useState<string | null>(null), [confirmSingle, setConfirmSingle] = useState<number | null>(null)
  const change = async (action: Action, unit_id?: string, revision = workflow?.revision ?? 0, reason?: string) => {
    setBusy(true); setError("")
    try {
      await api.workflowChange(root, { action, unit_id, expected_revision: revision, reason: reason ?? t(`views:workflow.reason.${action}`) })
      await loadItem(root, true)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      // a conflicting revision shows the newer graph before the user tries again
      await loadItem(root, true)
    } finally { setBusy(false) }
  }
  const units = workflow?.units ?? []
  const affected = rework ? (() => {
    const ids = new Set([rework])
    for (let grew = true; grew;) { grew = false; for (const u of units) if (!ids.has(u.id) && u.depends_on.some((id) => ids.has(id))) { ids.add(u.id); grew = true } }
    return orderedUnits(units).filter((u) => ids.has(u.id))
  })() : []
  const running = (s: string) => s === "running" || s === "dispatching"
  const activeAffected = affected.find((u) => running(u.status))
  const stoppingAffected = affected.some((u) => u.stop_pending)
  const otherActive = rework !== null && units.some((u) => !affected.some((a) => a.id === u.id) && running(u.status))
  const old = workflow?.current_cycle === false
  const dialogs = workflow && <>
    <AlertDialog open={confirmSingle !== null} onOpenChange={(open) => !open && setConfirmSingle(null)}>
      <AlertDialogContent className="max-h-[calc(100dvh-2rem)] w-[calc(100%-2rem)] grid-rows-[minmax(0,1fr)_auto] [overflow-wrap:anywhere]" data-workflow-single-confirm>
        <AlertDialogHeader className="min-h-0 min-w-0 overflow-y-auto">
          <AlertDialogTitle>{t("views:workflow.single")}</AlertDialogTitle>
          <AlertDialogDescription>{t("views:workflow.singleConfirm")}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter className="min-w-0 *:h-auto *:min-h-8 *:whitespace-normal">
          <AlertDialogCancel disabled={busy}>{t("common:action.cancel")}</AlertDialogCancel>
          <AlertDialogAction disabled={busy} onClick={() => { const revision = confirmSingle; setConfirmSingle(null); if (revision !== null) void change("single_session", undefined, revision) }} data-workflow-single-ok>{t("views:workflow.single")}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
    <AlertDialog open={rework !== null} onOpenChange={(open) => !open && setRework(null)}>
      <AlertDialogContent data-workflow-rework className="max-h-[calc(100dvh-2rem)] w-[calc(100%-2rem)] overflow-y-auto [overflow-wrap:anywhere]">
        <AlertDialogHeader><AlertDialogTitle>{t("views:workflow.reworkTitle")}</AlertDialogTitle><AlertDialogDescription>{t("views:workflow.reworkDesc")}</AlertDialogDescription></AlertDialogHeader>
        <ul className="list-disc space-y-1 pl-5 type-footnote">{affected.map((u) => <li key={u.id}>{u.title} · {t("views:workflow.budget", { n: u.attempt, max: workflow.max_attempts })}</li>)}</ul>
        {stoppingAffected && <p className="type-footnote text-attention">{t("views:workflow.stopPending")}</p>}
        {otherActive && !activeAffected && <p className="type-footnote text-attention">{t("views:workflow.otherActive")}</p>}
        <AlertDialogFooter><AlertDialogCancel disabled={busy}>{t("common:action.cancel")}</AlertDialogCancel>
          <AlertDialogAction disabled={busy || stoppingAffected || otherActive && !activeAffected || old || affected.some((u) => u.attempt >= workflow.max_attempts)} onClick={() => {
            const id = activeAffected?.id ?? rework
            if (!id) return
            void change(activeAffected ? "cancel" : "retry", id, workflow.revision, t(activeAffected ? "views:workflow.reason.cancel" : "views:workflow.reason.rework"))
            if (!activeAffected) setRework(null)
          }} data-workflow-rework-confirm>{activeAffected ? t("views:workflow.stopAffected") : t("views:workflow.confirmRework")}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </>
  return {
    root, tree, summary, workflow, shown, busy, error, dialogs, change,
    askRework: setRework, askSingle: () => workflow && setConfirmSingle(workflow.revision),
    stat: (item?: string) => item ? byItem.get(item) : undefined,
    total: (item?: string) => item ? totals.get(item) : undefined,
    single: workflow?.acceptance_mode === "single_session",
    unavailable: workflow?.execution_ready === false,
    old,
  }
}
export type AgentTreeState = ReturnType<typeof useAgentTree>

function collect(n: AgentNode, out: AgentNode[] = []): AgentNode[] { out.push(n); n.children.forEach((c) => collect(c, out)); return out }

/** an item's page shows the tree when it started anything or coordinates a work graph */
export const hasAgentTree = (at: AgentTreeState) => at.shown

const TONE: Record<AgentStatus, string> = {
  idle: "bg-muted text-muted-foreground",
  running: "bg-blue/10 text-blue", unconfirmed: "bg-blue/8 text-blue/80", starting: "bg-blue/8 text-blue/80",
  asking: "bg-attention/12 text-attention", blocked: "bg-attention/12 text-attention",
  done: "bg-success/10 text-success", failed: "bg-destructive/10 text-destructive",
  waiting: "bg-muted text-muted-foreground", pending: "bg-muted text-muted-foreground", paused: "bg-muted text-muted-foreground",
  stopped: "bg-muted text-muted-foreground", proposed: "bg-muted text-muted-foreground",
}
const STATUS_INK: Record<AgentStatus, string> = {
  idle: "text-muted-foreground",
  running: "text-blue", unconfirmed: "text-blue/80", starting: "text-blue/80", asking: "text-attention", blocked: "text-attention",
  done: "text-success", failed: "text-destructive", waiting: "text-muted-foreground", pending: "text-tertiary", paused: "text-muted-foreground",
  stopped: "text-tertiary", proposed: "text-muted-foreground",
}
const ICON: Record<AgentStatus, typeof CheckIcon> = {
  idle: CircleDashedIcon,
  running: LoaderCircleIcon, unconfirmed: LoaderCircleIcon, starting: LoaderCircleIcon, asking: MessageCircleQuestionIcon, blocked: TriangleAlertIcon,
  done: CheckIcon, failed: TriangleAlertIcon, waiting: HourglassIcon, pending: CircleDashedIcon, paused: CirclePauseIcon, stopped: CircleStopIcon, proposed: CircleDashedIcon,
}
export function AgentStatusPill({ s, className }: { s: AgentStatus; className?: string }) {
  const t = useT(), I = ICON[s]
  return <span className={cn("inline-flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-md px-1.5 type-label", TONE[s], className)} data-agent-status={s}>
    <I className={cn("size-3", isActive(s) && "motion-safe:animate-spin")} strokeWidth={2.25} />{t(`views:agentTree.status.${s}`)}</span>
}

/** what it cost: the estimate, "≥" when part of the tokens ran on models without a price, and only the token count
 * when none of them has a price (an estimate of "$0" there would be wrong); `hint` says which */
function costOf(c: Cost, t: ReturnType<typeof useT>): { text: string; hint?: string } {
  if (c.est >= 0.01 || (c.est > 0 && !c.unpriced)) return c.unpriced ? { text: "≥" + fmtUsd(c.est), hint: t("views:agentTree.unpricedPart", { n: fmtN(c.unpriced) }) } : { text: fmtUsd(c.est) }
  if (c.unpriced) return { text: t("views:agentTree.tokens", { n: fmtN(c.unpriced) }), hint: t("views:agentTree.unpricedAll") }
  return { text: fmtUsd(c.reported || c.est) }
}
const model = (bot: string | null) => bot && st.bots[bot] ? st.bots[bot].m || st.bots[bot].h : ""

/** cost and working time, one line; the hint explains an unpriced cost */
function figures(at: AgentTreeState, n: AgentNode, t: ReturnType<typeof useT>) {
  const cost = at.total(n.item), time = workTime(at.stat(n.item)), c = cost?.known ? costOf(cost, t) : null
  return { text: [c?.text, time ? clockDur(time) : null].filter(Boolean).join(" · "), hint: c?.hint }
}

function Card({ at, n, selected, onSelect, open, toggle }: { at: AgentTreeState; n: AgentNode; selected: boolean; onSelect: () => void; open: boolean; toggle: () => void }) {
  const t = useT()
  const kind = n.unit && n.unit.kind !== "work" ? t(`views:workflow.${n.unit.kind}`) : n.kind === "fork" ? t("views:agentTree.fork") : null
  const line = figures(at, n, t)
  return (
    <div role="button" tabIndex={0} aria-pressed={selected} onClick={onSelect} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onSelect() } }}
      className={cn("group/card @container/card anim-in relative w-full min-w-0 cursor-pointer rounded-xl bg-card p-3.5 text-left shadow-[var(--lift)] transition-[box-shadow,opacity] duration-[180ms] ease-[var(--ease-out)] hover:shadow-[var(--lift-hover)] focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-blue/30",
        selected && "ring-2 ring-blue", n.kind === "attempt" && "opacity-60 hover:opacity-100", n.status === "pending" && !selected && "bg-transparent shadow-none outline-1 -outline-offset-1 outline-dashed outline-border")}
      data-agent-node={n.key} data-agent-kind={n.kind}>
      <div className="flex min-w-0 items-start gap-2">
        <BotAvatar k={n.bot} size={28} className="mt-0.5" />
        <div className="min-w-0 flex-1">
          <div className="truncate type-callout font-medium">{n.bot ? botName(n.bot) : t("views:agentTree.noBot")}</div>
          <div className="truncate type-footnote text-tertiary">{model(n.bot) || " "}</div>
        </div>
        <AgentStatusPill s={n.status} className="@max-[200px]/card:hidden" />
      </div>
      <p className="mt-2.5 line-clamp-2 break-words type-callout text-foreground">
        {kind && <span className="mr-1.5 rounded-md bg-muted px-1.5 py-px type-footnote text-muted-foreground">{kind}</span>}{n.title}</p>
      {n.waitingOn.length > 0 && <p className="mt-1.5 truncate type-footnote text-muted-foreground" data-agent-waiting>{t("views:workflow.waitingFor", { names: n.waitingOn.join(" · ") })}</p>}
      {n.decisions > 0 && <p className="mt-1.5 type-footnote font-medium text-attention">{t("views:agentTree.asking", { count: n.decisions })}</p>}
      {n.kind === "attempt" && <p className="mt-1.5 type-footnote text-muted-foreground">{t("views:agentTree.attempt", { n: n.attempt })}</p>}
      <div className={cn("mt-2.5 min-h-5 items-center gap-2 type-footnote tabular-nums text-muted-foreground", line.text || n.children.length > 0 ? "flex" : "hidden @max-[200px]/card:flex")}>
        <AgentStatusPill s={n.status} className="hidden @max-[200px]/card:inline-flex" />
        <span className="min-w-0 flex-1 truncate" title={line.hint} data-agent-figures>{line.text}</span>
        {n.children.length > 0 && <button type="button" onClick={(e) => { e.stopPropagation(); toggle() }} onKeyDown={(e) => e.stopPropagation()} aria-expanded={open}
          aria-label={t(open ? "views:agentTree.collapse" : "views:agentTree.expand", { count: n.children.length })}
          className="-mr-1 inline-flex h-5 items-center gap-0.5 rounded-md px-1 hover:bg-accent hover:text-foreground" data-agent-toggle={n.key}>
          {n.children.length}{open ? <ChevronDownIcon className="size-3" /> : <ChevronRightIcon className="size-3" />}</button>}
      </div>
    </div>
  )
}

const LINE = "bg-border"
/** a card and, under it, a rail with everything it started */
function Branch(p: { at: AgentTreeState; n: AgentNode; selected: string | null; onSelect: (k: string) => void; closed: Set<string>; toggle: (k: string) => void }) {
  const { n, closed } = p, open = !closed.has(n.key)
  return <div className="min-w-0 max-w-[480px]">
    <Card at={p.at} n={n} selected={p.selected === n.key} onSelect={() => p.onSelect(n.key)} open={open} toggle={() => p.toggle(n.key)} />
    {open && n.children.length > 0 && <Rail {...p} nodes={n.children} />}
  </div>
}
function Rail(p: { at: AgentTreeState; nodes: AgentNode[]; selected: string | null; onSelect: (k: string) => void; closed: Set<string>; toggle: (k: string) => void }) {
  return <ul className="relative ml-4" data-agent-rail>
    <span aria-hidden className={cn("absolute -top-[3px] -left-[3px] size-[7px] rounded-full border border-tertiary/60 bg-background")} />
    {p.nodes.map((c, i) => {
      const last = i === p.nodes.length - 1
      return <li key={c.key} className="relative pt-2.5 pl-5">
        <span aria-hidden className={cn("absolute left-0 top-0 w-px", LINE)} style={{ height: last ? 34 : "100%" }} />
        <span aria-hidden className={cn("absolute left-0 h-px w-5", LINE)} style={{ top: 34 }} />
        <Branch {...p} n={c} />
      </li>
    })}
  </ul>
}

const COLUMN = 204, CELL = 236, GAP = 12, INDENT = 32
/** the coordinator, then its children: side by side under it while they fit in one row; in rows of a grid hung
 * from one trunk when there are more (a long work graph keeps the page's width instead of becoming a column);
 * on a narrow page, a rail */
function Canvas({ at, selected, onSelect }: { at: AgentTreeState; selected: string | null; onSelect: (k: string) => void }) {
  const ref = useRef<HTMLDivElement>(null), [width, setWidth] = useState(0)
  const [closed, setClosed] = useState<Set<string>>(new Set())
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const ro = new ResizeObserver(() => setWidth(el.clientWidth))
    ro.observe(el); setWidth(el.clientWidth)
    return () => ro.disconnect()
  }, [])
  const root = at.tree!
  const toggle = (k: string) => setClosed((s) => { const x = new Set(s); if (x.has(k)) x.delete(k); else x.add(k); return x })
  const open = !closed.has(root.key), kids = open ? root.children : []
  const cols = Math.max(1, Math.floor((width - INDENT + GAP) / (CELL + GAP)))
  const mode = kids.length > 1 && width >= kids.length * COLUMN ? "bus" : kids.length > 1 && cols >= 2 ? "grid" : "rail"
  const props = { at, selected, onSelect, closed, toggle }
  const rootCard = <Card at={at} n={root} selected={selected === root.key} onSelect={() => onSelect(root.key)} open={open} toggle={() => toggle(root.key)} />
  const dot = "absolute size-[7px] rounded-full border border-tertiary/60 bg-background"
  return <div ref={ref} className="min-w-0" data-agent-canvas={mode}>
    {mode === "bus" ? <div className="flex flex-col items-center">
      <div className="w-full max-w-[300px]">{rootCard}</div>
      <span aria-hidden className={cn("relative h-5 w-px", LINE)}><span className={cn(dot, "-top-[3px] -left-[3px]")} /></span>
      <div className="flex w-full" data-agent-bus>
        {kids.map((c, i) => <div key={c.key} className="relative min-w-0 flex-1 px-1.5 pt-5">
          <span aria-hidden className={cn("absolute top-0 h-px", LINE)} style={{ left: i === 0 ? "50%" : 0, right: i === kids.length - 1 ? "50%" : 0 }} />
          <span aria-hidden className={cn("absolute top-0 left-1/2 h-5 w-px", LINE)} />
          <Branch {...props} n={c} />
        </div>)}
      </div>
    </div> : mode === "grid" ? (() => {
      // rows of `cols` cells; each row's bus runs from the trunk to the middle of its last cell
      const cell = (width - INDENT - (cols - 1) * GAP) / cols
      const rows = Array.from({ length: Math.ceil(kids.length / cols) }, (_, r) => kids.slice(r * cols, r * cols + cols))
      return <div data-agent-grid>
        <div className="max-w-[320px]">{rootCard}</div>
        {rows.map((row, r) => <div key={r} className="relative pt-4" style={{ paddingLeft: INDENT }}>
          {r === 0 && <span aria-hidden className={cn(dot, "top-[-3px]")} style={{ left: 13 }} />}
          <span aria-hidden className={cn("absolute top-0 w-px", LINE)} style={{ left: 16, height: r === rows.length - 1 ? 9 : "100%" }} />
          <span aria-hidden className={cn("absolute top-2 h-px", LINE)} style={{ left: 16, width: INDENT - 16 + (row.length - 1) * (cell + GAP) + cell / 2 }} />
          <div className="grid items-start" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`, columnGap: GAP, rowGap: GAP }}>
            {row.map((c) => <div key={c.key} className="relative min-w-0">
              <span aria-hidden className={cn("absolute -top-2 left-1/2 h-2 w-px", LINE)} />
              <Branch {...props} n={c} />
            </div>)}
          </div>
        </div>)}
      </div>
    })() : <Branch {...props} n={root} />}
  </div>
}

/** the header line, notices, the tree and the graph's change history */
/** the workflow's own controls: pause or resume the graph, and, while paused, switch to single-session acceptance */
export function WorkflowButtons({ at }: { at: AgentTreeState }) {
  const t = useT(), workflow = at.workflow
  if (!workflow || at.single || at.old) return null
  return <div className="flex items-center gap-1">
    <Button size="sm" variant="outline" disabled={at.busy || workflow.paused && (at.unavailable || at.old)} onClick={() => void at.change(workflow.paused ? "resume" : "pause")} data-workflow-toggle>
      {workflow.paused ? <PlayIcon /> : <PauseIcon />}{workflow.paused ? t("views:workflow.resume") : t("views:workflow.pause")}</Button>
    {workflow.paused && <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-agent-more />}><MoreHorizontalIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-44"><DropdownMenuItem disabled={at.busy} onClick={at.askSingle} data-workflow-single>{t("views:workflow.single")}</DropdownMenuItem></DropdownMenuContent>
    </DropdownMenu>}
  </div>
}

/** current failures (with retry) and cards waiting for you under this item, as two small pills */
export function TreePills({ at, onSelect }: { at: AgentTreeState; onSelect: (k: string) => void }) {
  const t = useT(), { summary, workflow } = at
  if (!summary) return null
  const canAct = !!workflow && !at.single
  return <>
    {summary.failed.length > 0 && <Popover>
      <PopoverTrigger render={<button type="button" className="inline-flex h-6 items-center gap-1 rounded-md bg-destructive/10 px-2 type-label text-destructive hover:bg-destructive/14" data-agent-failed />}>
        {t("views:agentTree.failed", { count: summary.failed.length })}<ChevronDownIcon className="size-3" /></PopoverTrigger>
      <PopoverContent align="end" className="w-80 gap-3 rounded-xl p-3" data-agent-failed-list>
        {summary.failed.map((n) => <div key={n.key} className="flex flex-col gap-1.5">
          <div className="flex items-start gap-2 type-callout"><TriangleAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" /><div className="min-w-0"><div className="break-words font-medium">{n.title}</div>{n.reason && <div className="mt-0.5 break-words type-footnote text-muted-foreground">{n.reason}</div>}</div></div>
          <div className="flex justify-end gap-2">
            <Button size="sm" variant="ghost" onClick={() => onSelect(n.key)}>{t("views:agentTree.details")}</Button>
            {canAct && n.unit && <Button size="sm" disabled={at.busy || at.unavailable || at.old || n.unit.attempt >= workflow!.max_attempts} onClick={() => void at.change("retry", n.unit!.id)} data-workflow-retry={n.unit.id}><RotateCcwIcon />{t("views:workflow.retry")}</Button>}
          </div>
        </div>)}
      </PopoverContent>
    </Popover>}
    {summary.asking.length > 0 && <Popover>
      <PopoverTrigger render={<button type="button" className="inline-flex h-6 items-center gap-1 rounded-md bg-attention/12 px-2 type-label text-attention hover:bg-attention/14" data-workflow-decisions />}>
        {t("views:agentTree.askingAll", { count: summary.asking.reduce((s, n) => s + n.decisions, 0) })}<ChevronDownIcon className="size-3" /></PopoverTrigger>
      <PopoverContent align="end" className="w-72 gap-1 rounded-xl p-1.5">
        {summary.asking.map((n) => <a key={n.key} href={`#/items/${encodeURIComponent(n.item!)}`} className="rounded-lg px-2 py-1.5 type-callout hover:bg-accent">{n.title} · {t("views:workflow.handleDecision")}</a>)}
      </PopoverContent>
    </Popover>}
  </>
}

/** the header line, notices, the tree and the graph's change history; `controls` off when the item page shows
 * the notices and buttons itself (the full tree opened from its side column) */
export function AgentTree({ at, selected, onSelect, controls = true }: { at: AgentTreeState; selected: string | null; onSelect: (k: string | null) => void; controls?: boolean }) {
  const t = useT()
  const { tree, summary, workflow } = at
  if (!tree || !summary) return null
  const total = at.total(tree.item)
  return <section className="min-w-0" data-agent-tree data-workflow={workflow ? "" : undefined}>
    <div className="mb-4 flex flex-wrap items-center gap-x-3 gap-y-2" data-agent-header>
      <h2 className="flex items-center gap-2 type-headline"><NetworkIcon className="size-4 text-muted-foreground" />{t("views:agentTree.title")}</h2>
      <span className="flex-1" />
      <span className="type-footnote tabular-nums text-muted-foreground" data-agent-count>{t("views:agentTree.agents", { count: summary.agents })}</span>
      {summary.active > 0 && <span className="type-footnote tabular-nums text-blue">{t("views:agentTree.active", { count: summary.active })}</span>}
      <TreePills at={at} onSelect={onSelect} />
      {total?.known && (() => { const c = costOf(total, t); return <span className="type-footnote tabular-nums text-foreground" title={c.hint} data-agent-total>{t("views:agentTree.cost", { usd: c.text })}</span> })()}
      {controls && <WorkflowButtons at={at} />}
    </div>
    {controls && at.dialogs}
    {controls && <Notices at={at} />}
    <Canvas at={at} selected={selected} onSelect={(k) => onSelect(k === selected ? null : k)} />
    {workflow && <History workflow={workflow} />}
  </section>
}

/** the item page's side column: everyone working on it, one row each, in the tree's order; a running agent's row
 * moves, the rest is quiet. The full tree opens from here. */
export function TeamPanel({ at, selected, onSelect, onExpand }: { at: AgentTreeState; selected: string | null; onSelect: (k: string | null) => void; onExpand: () => void }) {
  const t = useT()
  const { tree, summary } = at
  if (!tree || !summary) return null
  const total = at.total(tree.item), c = total?.known ? costOf(total, t) : null
  const row = (n: AgentNode, depth: number): ReactNode => {
    const I = ICON[n.status], f = figures(at, n, t), on = selected === n.key
    return <li key={n.key}>
      <button type="button" onClick={() => onSelect(on ? null : n.key)} aria-pressed={on} data-agent-node={n.key} data-agent-kind={n.kind}
        className={cn("group flex w-full min-w-0 items-start gap-2 rounded-lg py-1.5 pr-1.5 text-left transition-colors hover:bg-accent", on && "bg-accent", n.kind === "attempt" && "opacity-55 hover:opacity-100")}
        style={{ paddingLeft: 6 + depth * 14 }}>
        <I className={cn("mt-[3px] size-3.5 shrink-0", STATUS_INK[n.status], isActive(n.status) && "motion-safe:animate-spin")} strokeWidth={2.25} aria-label={t(`views:agentTree.status.${n.status}`)} />
        <span className="min-w-0 flex-1">
          <span className="block truncate type-callout text-foreground">{n.unit && n.unit.kind !== "work" && <span className="mr-1 text-muted-foreground">{t(`views:workflow.${n.unit.kind}`)} ·</span>}{n.title}</span>
          <span className="block truncate type-footnote text-tertiary" title={f.hint}>{[n.bot ? botName(n.bot) : null, n.kind === "attempt" ? t("views:agentTree.attempt", { n: n.attempt }) : null, f.text].filter(Boolean).join(" · ")}</span>
        </span>
        {n.decisions > 0 && <span className="mt-0.5 inline-flex h-5 min-w-5 shrink-0 items-center justify-center rounded-md bg-attention/12 px-1 type-label tabular-nums text-attention">{n.decisions}</span>}
      </button>
      {n.children.length > 0 && <ul>{n.children.map((k) => row(k, depth + 1))}</ul>}
    </li>
  }
  return <section className="min-w-0" data-agent-team>
    <div className="mb-2 flex items-center gap-2">
      <h3 className="type-label text-tertiary">{t("views:agentTree.team")}</h3>
      <span className="type-footnote tabular-nums text-tertiary">{t("views:agentTree.agents", { count: summary.agents })}{summary.active > 0 && <span className="text-blue"> · {t("views:agentTree.active", { count: summary.active })}</span>}</span>
      <span className="flex-1" />
      <Button variant="ghost" size="xs" className="text-muted-foreground" onClick={onExpand} data-agent-expand>{t("views:agentTree.showTree")}</Button>
    </div>
    {(summary.failed.length > 0 || summary.asking.length > 0) && <div className="mb-1.5 flex flex-wrap gap-1.5"><TreePills at={at} onSelect={(k) => onSelect(k)} /></div>}
    <ul className="-mx-1.5" data-agent-rows>{tree.children.map((n) => row(n, 0))}</ul>
    {c && <p className="mt-2 type-footnote tabular-nums text-tertiary" title={c.hint} data-agent-total>{t("views:agentTree.totalWithChildren", { usd: c.text })}</p>}
  </section>
}

function Notice({ mark, children }: { mark?: string; children: ReactNode }) {
  return <p role="status" className="mb-3 break-words rounded-xl bg-muted/60 px-4 py-2.5 type-callout text-muted-foreground" {...(mark ? { [`data-${mark}`]: "" } : {})}>{children}</p>
}
/** what the work graph's state means and what keeps the parent from finishing; `inline` inside the item page's
 * "now" card (plain lines under its headline, which already names the state) instead of boxes of their own */
export function Notices({ at, inline = false }: { at: AgentTreeState; inline?: boolean }) {
  const t = useT(), w = at.workflow
  if (!w) return null
  const N = inline ? ({ mark, children }: { mark?: string; children: ReactNode }) => <p className="break-words type-callout text-muted-foreground" {...(mark ? { [`data-${mark}`]: "" } : {})}>{children}</p> : Notice
  return <>
    {at.error && <p role="alert" className={cn("break-words type-callout text-destructive", !inline && "mb-3")}>{at.error}</p>}
    {at.single && <N mark="workflow-single-mode">{t("views:workflow.singleHint")}</N>}
    {at.unavailable && !at.single && !(inline && at.old) && <N mark="workflow-unavailable">{t("views:workflow.unavailable")}</N>}
    {at.old && <N mark="workflow-prior-cycle">{t("views:workflow.priorCycle")}</N>}
    {w.paused && !at.old && !at.single && !at.unavailable && <N>{t("views:workflow.pauseHint")}</N>}
    {w.blockers.length > 0 && <details className={cn("group", inline ? "" : "mb-3 rounded-xl bg-muted/60 px-4 py-2.5")} data-workflow-blockers>
      <summary className="flex cursor-pointer list-none items-center gap-1.5 type-callout"><ChevronRightIcon className="size-3.5 transition-transform duration-[180ms] group-open:rotate-90" />{t("views:workflow.blockers")} · {w.blockers.length}</summary>
      <ul className="mt-1.5 list-disc space-y-1 pl-6 type-callout text-muted-foreground">{w.blockers.map((text, n) => <li key={n} className="break-words">{text}</li>)}</ul>
    </details>}
  </>
}

export function History({ workflow }: { workflow: Workflow }) {
  const t = useT()
  const older = (workflow.history_attempts_total ?? 0) - (workflow.history_attempts?.length ?? 0)
  if (!workflow.history.length && older <= 0) return null
  return <details className="group mt-4" data-workflow-history>
    <summary className="flex cursor-pointer list-none items-center gap-1.5 type-footnote text-muted-foreground hover:text-foreground"><ChevronRightIcon className="size-3.5 transition-transform duration-[180ms] group-open:rotate-90" />{t("views:workflow.history", { count: workflow.history_total ?? workflow.history.length })}</summary>
    {older > 0 && <p className="mt-2 type-footnote text-muted-foreground">{t("views:workflow.historyAttemptsShown", { shown: workflow.history_attempts?.length ?? 0, total: workflow.history_attempts_total })}</p>}
    <ol className="mt-2 ml-1.5 space-y-2 border-l pl-3 type-footnote">{workflow.history.slice().reverse().map((entry, n) => <li key={n} className="break-words"><span className="text-muted-foreground">{t("views:workflow.revision", { n: entry.revision })} · </span>{entry.reason}</li>)}</ol>
  </details>
}

/** the selected agent: what it was asked, what it reported, and what can be done with it */
export function AgentDetail({ at, node, onClose }: { at: AgentTreeState; node: AgentNode; onClose: () => void }) {
  const t = useT()
  const w = at.workflow, u = node.kind === "unit" ? node.unit : undefined
  const canAct = !!w && !at.single && !!u
  const running = !!u && (u.status === "running" || u.status === "dispatching")
  const line = figures(at, node, t)
  const item = node.item ? st.items.find((i) => i.id === node.item) : undefined
  return <div className="flex min-w-0 flex-col gap-4" data-agent-detail={node.key}>
    <div className="flex items-start gap-2.5">
      <BotAvatar k={node.bot} size={36} />
      <div className="min-w-0 flex-1">
        <div className="truncate type-headline">{node.bot ? botName(node.bot) : t("views:agentTree.noBot")}</div>
        <div className="truncate type-footnote text-muted-foreground">{model(node.bot)}</div>
      </div>
      <Button variant="ghost" size="icon-sm" className="-mr-1 text-muted-foreground" aria-label={t("common:action.close")} onClick={onClose} data-agent-detail-close><XIcon /></Button>
    </div>
    <div>
      <div className="flex flex-wrap items-center gap-2"><AgentStatusPill s={node.status} />{line.text && <span className="type-footnote tabular-nums text-muted-foreground" data-agent-figures>{line.text}</span>}</div>
      {line.hint && <p className="mt-1 type-footnote text-muted-foreground">{line.hint}</p>}
      <h3 className="mt-3 break-words type-body font-medium">{node.title}</h3>
      {node.kind === "attempt" && <p className="mt-1 type-footnote text-muted-foreground">{t("views:workflow.priorAttempt")}</p>}
      {node.kind === "fork" && item?.parent?.item && <p className="mt-1 type-footnote text-muted-foreground">{t("views:agentTree.forkedFrom", { id: item.parent.item })}</p>}
    </div>
    <div className="flex flex-wrap gap-2">
      {node.item && <Button size="sm" variant="outline" nativeButton={false} render={<a href={`#/items/${encodeURIComponent(node.item)}`} />} data-agent-open-item>{node.kind === "root" ? t("views:agentTree.openItem") : t("views:workflow.openChild")}</Button>}
      {node.session && <Button size="sm" variant="outline" nativeButton={false} render={<a href={`#/sessions/${encodeURIComponent(node.session)}`} />}>{t("views:workflow.openSession")}</Button>}
      {canAct && running && <Button size="sm" variant="outline" disabled={at.busy || at.old} onClick={() => void at.change("cancel", u!.id)} data-workflow-cancel={u!.id}><SquareIcon />{t("views:workflow.cancelUnit")}</Button>}
      {canAct && u!.status === "succeeded" && <Button size="sm" variant="outline" disabled={at.busy || at.old || u!.attempt >= w!.max_attempts} onClick={() => at.askRework(u!.id)} data-workflow-rework={u!.id}><RotateCcwIcon />{t("views:workflow.rework")}</Button>}
      {canAct && u!.status === "failed" && <Button size="sm" disabled={at.busy || at.unavailable || at.old || u!.attempt >= w!.max_attempts} onClick={() => void at.change("retry", u!.id)} data-workflow-retry={u!.id}><RotateCcwIcon />{t("views:workflow.retry")}</Button>}
    </div>
    {u && w && <dl className="grid grid-cols-[72px_minmax(0,1fr)] gap-x-3 gap-y-1.5 type-callout" data-agent-unit={u.id}>
      <dt className="text-muted-foreground">{t("views:agentTree.kind")}</dt><dd>{t(`views:workflow.${u.kind}`)}{!u.required && ` · ${t("views:workflow.optional")}`}</dd>
      <dt className="text-muted-foreground">{t("views:agentTree.attempts")}</dt><dd className="tabular-nums">{t("views:workflow.budget", { n: u.attempt, max: w.max_attempts })}</dd>
      {u.resources.length > 0 && <><dt className="text-muted-foreground">{t("views:agentTree.scope")}</dt><dd className="break-words">{u.resources.join(" · ")}</dd></>}
    </dl>}
    {u && <div className="flex flex-col gap-1.5 type-footnote">
      {node.waitingOn.length > 0 && <p className="text-muted-foreground">{t("views:workflow.waitingFor", { names: node.waitingOn.join(" · ") })}</p>}
      {running && u.activity?.state !== "confirmed" && <p className="text-muted-foreground">{u.activity?.reason ?? t("views:workflow.activityUnknown")}</p>}
      {u.stop_pending && <p className="text-attention">{t("views:workflow.stopPending")}</p>}
      {st.bots[u.bot_id]?.on === false && <p className="text-attention">{t("views:workflow.botDisabled")}</p>}
    </div>}
    {node.reason && <p className="break-words rounded-lg bg-muted/60 px-3 py-2 type-callout">{node.reason}</p>}
    {u?.goal && <section><h4 className="mb-1.5 type-label text-tertiary">{t("views:agentTree.goal")}</h4><p className="whitespace-pre-wrap break-words type-callout">{u.goal}</p></section>}
    {u?.summary && <section><h4 className="mb-1.5 type-label text-tertiary">{t("views:agentTree.summary")}</h4><p className="whitespace-pre-wrap break-words type-callout">{u.summary}</p></section>}
    {!!u?.outputs?.length && <section data-workflow-outputs><h4 className="mb-1.5 type-label text-tertiary">{t("views:workflow.outputs")}</h4>
      {u.outputs.map((o, n) => <div key={n} className="mt-1 type-callout"><span className="mr-2 text-muted-foreground">{t(o.role === "record" ? "views:workflow.outputRecord" : o.role === "result" ? "views:workflow.outputResult" : "views:workflow.outputUnspecified")}</span><span>{o.title}</span>{o.location && <p className="break-all type-code text-muted-foreground">{o.location}</p>}</div>)}</section>}
    {!!u?.checks?.length && <ul className="space-y-1 type-footnote">{u.checks.map((c, n) => <li key={n} className={cn("flex items-start gap-1.5", !c.passed && "text-destructive")}><CircleCheckIcon className="mt-0.5 size-3 shrink-0" /><span>{c.name} · {c.passed ? t("views:workflow.checkPassed") : t("views:workflow.checkFailed")}{c.operated && ` · ${t("views:workflow.operated")}`}</span></li>)}</ul>}
    {node.children.length > 0 && <p className="flex items-center gap-1.5 type-footnote text-muted-foreground"><GitForkIcon className="size-3.5" />{t("views:agentTree.started", { count: node.children.length })}</p>}
    {isOver(node.status) && !line.text && <p className="type-footnote text-muted-foreground">{t("views:agentTree.noFigures")}</p>}
  </div>
}

/** keeps a selection only while its node is still in the tree */
export function useSelectedNode(at: AgentTreeState) {
  const [sel, setSel] = useState<string | null>(null)
  const node = sel && at.tree ? findNode(at.tree, sel) : null
  useEffect(() => { if (sel && at.tree && !node) setSel(null) }, [sel, node, at.tree])
  return [node, (k: string | null) => setSel(k)] as const
}

/** the selected agent beside the page, which stays usable (another card can be picked while it is open); a
 * sheet from the bottom on narrow pages */
export function AgentDetailPanel({ at, node, onClose }: { at: AgentTreeState; node: AgentNode | null; onClose: () => void }) {
  const wide = useWide()
  useEffect(() => {
    if (!node || !wide) return
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape" && !document.querySelector("[role=alertdialog],[role=dialog]")) onClose() }
    addEventListener("keydown", esc)
    return () => removeEventListener("keydown", esc)
  }, [node, wide, onClose])
  if (!node) return null
  if (wide) return <aside className="fixed top-16 right-4 bottom-4 z-30 w-[380px] overflow-y-auto rounded-2xl bg-popover p-5 text-popover-foreground shadow-xl motion-safe:animate-in motion-safe:fade-in-0 motion-safe:slide-in-from-right-4 motion-safe:duration-[260ms] motion-safe:ease-[var(--ease-out)]" aria-label={node.title} data-agent-panel>
    <AgentDetail at={at} node={node} onClose={onClose} /></aside>
  return <Sheet open onOpenChange={(open) => !open && onClose()}>
    <SheetContent side="bottom" className="max-h-[85dvh] overflow-y-auto rounded-t-2xl p-4 pb-[max(1rem,env(safe-area-inset-bottom))]" showCloseButton={false} data-agent-panel>
      <SheetTitle className="sr-only">{node.title}</SheetTitle><AgentDetail at={at} node={node} onClose={onClose} /></SheetContent></Sheet>
}

/** the page is wide enough for a panel beside its content (Tailwind `lg`) */
function useWide() {
  const query = "(min-width: 1024px)"
  const [wide, setWide] = useState(() => matchMedia(query).matches)
  useEffect(() => {
    const mql = matchMedia(query), on = () => setWide(mql.matches)
    mql.addEventListener("change", on); on()
    return () => mql.removeEventListener("change", on)
  }, [])
  return wide
}
