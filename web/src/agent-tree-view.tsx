/* ── the item page's collaboration area ──
 * One row per step (affectUI Parallel Attempts, without its picking): who ran it, what it is doing or how it ended,
 * a bar for how far it got, its working time and cost, and the one thing to do about it (Away Summary's way: decide,
 * retry, open). A step's earlier tries fold into its row. The header counts what needs you, failed, runs and is done,
 * with the coordinator, the time it took against the time its agents worked, and the estimated cost. A row picked
 * shows its detail beside the page (a sheet on narrow pages), which lists the tries. "查看全图" opens the steps as a
 * dependency graph in a large dialog (collab-graph.tsx). */
import { lazy, Suspense, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { BanIcon, CheckIcon, ChevronDownIcon, ChevronRightIcon, CircleCheckIcon, CircleDashedIcon, CirclePauseIcon, GitForkIcon, HourglassIcon, LoaderCircleIcon, MessageCircleQuestionIcon, MoreHorizontalIcon, PauseIcon, PlayIcon, RotateCcwIcon, SquareIcon, TriangleAlertIcon, WorkflowIcon, XIcon } from "lucide-react"
import { api } from "@/api"
import { stats as statsApi } from "@/api/stats"
import { Button } from "@/components/ui/button"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet"
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"
import { useT } from "@/i18n"
import { clockDur, longDur } from "@/i18n/format"
import { cn } from "@/lib/utils"
import { fmtN, fmtUsd } from "@/stats/format"
import { botName, loadItem, st } from "@/store"
import { BotAvatar } from "@/ui"
import { Markdown } from "@/components/markdown"
import { OutDialog, type OutOpen } from "@/item-outputs"
import { orderedUnits, stopPending, type Workflow } from "@/workflow"
import type { ItemState } from "@/item-state"
import { buildAgentTree, workTime, findNode, isActive, isOver, rollup, summarize, teamLanes, type AgentNode, type AgentStatus, type Cost, type TeamRow, type TreeItem, type TreeStat } from "@/agent-tree"
import { ParallelAttempts, type Attempt, type AttemptStatus, type PacLabels } from "@/components/affect/parallel-attempts-clean"

/** the tree's root takes the item page's one state (item-state.ts) */
export const ROOT_STATUS: Record<ItemState["kind"], AgentStatus> = {
  done: "done", dropped: "stopped", failed: "failed", proposed: "proposed", asking: "asking", unavailable: "blocked",
  graphPaused: "paused", paused: "paused", blocked: "blocked", working: "running", queued: "idle", idle: "idle",
}

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
  const stoppingAffected = affected.some(stopPending)
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
    root, tree, summary, workflow, shown, busy, error, dialogs, change, stats: rows,
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
const ICON: Record<AgentStatus, typeof CheckIcon> = {
  idle: CircleDashedIcon,
  running: LoaderCircleIcon, unconfirmed: LoaderCircleIcon, starting: LoaderCircleIcon, asking: MessageCircleQuestionIcon, blocked: TriangleAlertIcon,
  done: CheckIcon, failed: TriangleAlertIcon, waiting: HourglassIcon, pending: CircleDashedIcon, paused: CirclePauseIcon, stopped: BanIcon, proposed: CircleDashedIcon,
}
export function AgentStatusPill({ s, className }: { s: AgentStatus; className?: string }) {
  const t = useT(), I = ICON[s]
  return <span className={cn("inline-flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-md px-1.5 type-label", TONE[s], className)} data-agent-status={s}>
    <I className={cn("size-3", isActive(s) && "motion-safe:animate-spin")} strokeWidth={2.25} />{t(`views:agentTree.status.${s}`)}</span>
}

/** what it cost: the estimate, "≥" when part of the tokens ran on models without a price, and only the token count
 * when none of them has a price (an estimate of "$0" there would be wrong); `hint` says which */
export function costOf(c: Cost, t: ReturnType<typeof useT>): { text: string; hint?: string } {
  if (c.est >= 0.01 || (c.est > 0 && !c.unpriced)) return c.unpriced ? { text: "≥" + fmtUsd(c.est), hint: t("views:agentTree.unpricedPart", { n: fmtN(c.unpriced) }) } : { text: fmtUsd(c.est) }
  if (c.unpriced) return { text: t("views:agentTree.tokens", { n: fmtN(c.unpriced) }), hint: t("views:agentTree.unpricedAll") }
  return { text: fmtUsd(c.reported || c.est) }
}
export const model = (bot: string | null) => bot && st.bots[bot] ? st.bots[bot].m || st.bots[bot].h : ""

/** cost and working time, one line; the hint explains an unpriced cost */
function figures(at: AgentTreeState, n: AgentNode, t: ReturnType<typeof useT>) {
  const cost = at.total(n.item), time = workTime(at.stat(n.item)), c = cost?.known ? costOf(cost, t) : null
  return { text: [c?.text, time ? clockDur(time) : null].filter(Boolean).join(" · "), hint: c?.hint }
}

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

/** the collaboration with the work graph's own controls, notices and change history (a page without its own "now"
 * card shows them here) */
export function AgentTree({ at, selected, onSelect, controls = true }: { at: AgentTreeState; selected: string | null; onSelect: (k: string | null) => void; controls?: boolean }) {
  if (!at.tree || !at.summary) return null
  return <section className="min-w-0" data-agent-tree data-workflow={at.workflow ? "" : undefined}>
    {controls && at.dialogs}
    {controls && <Notices at={at} />}
    <TeamSteps at={at} selected={selected} onSelect={onSelect} actions={controls && <WorkflowButtons at={at} />} />
    {at.workflow && <History workflow={at.workflow} />}
  </section>
}

/** re-renders while something runs, so a running step's bar and time move on */
function useNow(live: boolean) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => { if (!live) return; setNow(Date.now()); const id = setInterval(() => setNow(Date.now()), 10_000); return () => clearInterval(id) }, [live])
  return now
}

/** a step's look in the list: the bar's colour and the row's state */
const ROW: Record<AgentStatus, AttemptStatus> = {
  done: "done", running: "running", starting: "running", unconfirmed: "running", asking: "waiting", blocked: "queued", failed: "failed", stopped: "stopped",
  paused: "queued", idle: "queued", proposed: "queued", pending: "queued", waiting: "queued",
}
/** done steps shown before "show the other N steps": the latest few */
const DONE_SHOWN = 3
export const sumCost = (cs: (Cost | undefined)[]): Cost | undefined => cs.reduce<Cost | undefined>((s, c) => !c ? s : !s ? { ...c } : { est: s.est + c.est, unpriced: s.unpriced + c.unpriced, reported: s.reported + c.reported, tokens: s.tokens + c.tokens, known: s.known || c.known }, undefined)
/** one line of plain text: Markdown's marks and code fences dropped */
const clip1 = (s: string) => s.replace(/```[\s\S]*?```/g, " ").replace(/^\s*#+\s*/gm, "").replace(/[`*_>#|]/g, "").replace(/\s+/g, " ").trim()

/** the step list's words (Parallel Attempts without its picking), translated */
export const stepLabels = (t: ReturnType<typeof useT>): Partial<PacLabels> => ({ list: t("views:agentTree.title"), working: t("views:agentTree.status.running"),
  status: { running: t("views:agentTree.status.running"), done: t("views:agentTree.status.done"), failed: t("views:agentTree.status.failed"), waiting: t("views:agentTree.status.asking"), queued: t("views:agentTree.status.pending"), stopped: t("views:agentTree.status.stopped") } })

/** one step of the collaboration as the list shows it: its state, what it says, how far it is, what it took */
export type StepView = { row: TeamRow; node: AgentNode; look: AttemptStatus; needsYou: boolean; question: string | null; line: string; progress: number; end: number; cost?: Cost }
/** the steps in the order of the work, each with its state line and its bar: needs you first among the states, a
 * running step's length from its plan when it has one, else from how long it has run against the steps already done */
export function stepViews(at: AgentTreeState, rows: TeamRow[], now: number, t: ReturnType<typeof useT>): StepView[] {
  const doneMs = rows.filter((r) => r.node.status === "done" && r.ms > 0).map((r) => r.ms).sort((a, b) => a - b)
  const typical = doneMs.length ? doneMs[Math.floor(doneMs.length / 2)] : 15 * 60_000
  const byTime = (ms: number) => Math.min(0.92, Math.max(0.04, ms / (typical * 1.25)))
  return rows.map((row) => {
    const n = row.node
    const item = n.item ? st.items.find((i) => i.id === n.item) : undefined
    const plan = item?.plan?.length ? item.plan.filter((x) => x[1] !== 0).length / item.plan.length : null
    const needsYou = n.decisions > 0 && !isOver(n.status)
    const look: AttemptStatus = needsYou ? "waiting" : ROW[n.status]
    const seg = row.segments.at(-1)
    const ran = isActive(n.status) && seg ? now - seg.start : row.ms
    const progress = look === "done" ? 1 : look === "queued" ? 0 : plan ?? (ran > 0 ? byTime(ran) : 0.5)
    const question = needsYou ? item?.waits[0]?.q ?? null : null
    const tried = row.tries.length
    const u = n.unit
    const said = clip1(u?.summary ?? "")
    const line = needsYou ? (question ? t("views:agentTree.row.needsYou", { q: clip1(question) }) : t("views:agentTree.row.needsYouBare"))
      : n.status === "done" ? [tried ? t("views:agentTree.passedOn", { n: n.attempt ?? tried + 1 }) : null, said || (tried ? null : t("views:agentTree.status.done"))].filter(Boolean).join(" · ")
      : n.status === "failed" ? (n.reason ? t("views:agentTree.row.failed", { reason: clip1(n.reason) }) : t("views:agentTree.row.failedBare"))
      : isActive(n.status) ? clip1(item?.now || (n.status === "unconfirmed" ? u?.activity?.reason ?? "" : "")) || t(`views:agentTree.status.${n.status}`)
      : n.waitingOn.length ? t("views:workflow.waitingFor", { names: n.waitingOn.join(" · ") })
      : n.status === "pending" ? t("views:agentTree.notStarted")
      : [t(`views:agentTree.status.${n.status}`), n.reason ? clip1(n.reason) : null].filter(Boolean).join(" · ")
    const retried = tried && n.status !== "done" ? ` · ${t("views:agentTree.retried", { count: tried })}` : ""
    const end = seg?.end ?? 0
    return { row, node: n, look, needsYou, question, line: line + retried, progress, end, cost: sumCost([...row.tries, n].map((x) => at.total(x.item))) }
  })
}

/** the item page's collaboration: one row per step (its earlier tries folded into it) – who ran it, what it is doing
 * or how it ended, a bar for how far it got, its working time and cost, and the one thing to do about it. What needs
 * you, failed or runs always shows; of the steps done only the latest few, the rest a click away. A row picked shows
 * its detail beside the page; "查看全图" opens the steps as a dependency graph. */
export function TeamSteps({ at, selected, onSelect, actions }: { at: AgentTreeState; selected: string | null; onSelect: (k: string | null) => void; actions?: ReactNode }) {
  const t = useT()
  const { tree, summary } = at
  const now = useNow(!!summary?.active && !at.old)
  const [box, beside] = usePanelRoom(!!selected)
  const [all, setAll] = useState(false)
  const [graph, setGraph] = useState(false)
  if (!tree || !summary) return null
  const lanes = teamLanes(tree, at.stats, now, at.old)
  const { range } = lanes
  const total = at.total(tree.item), c = total?.known ? costOf(total, t) : null
  const steps = stepViews(at, lanes.rows, now, t)
  /* the latest few done steps by when they ended; everything not done always shows */
  const done = steps.filter((x) => x.look === "done").sort((a, b) => b.end - a.end)
  const hidden = new Set(all ? [] : done.slice(DONE_SHOWN).map((x) => x.node.key))
  const shown = steps.filter((x) => !hidden.has(x.node.key))
  const canAct = !!at.workflow && !at.single
  const action = (x: StepView) => {
    const n = x.node, u = n.unit, w = at.workflow
    const open = n.item && n.item !== at.root ? `#/items/${encodeURIComponent(n.item)}` : n.session ? `#/sessions/${encodeURIComponent(n.session)}` : null
    if (x.needsYou && n.item) return <Button size="xs" variant="outline" nativeButton={false} render={<a href={`#/items/${encodeURIComponent(n.item)}`} />} data-step-action="decide">{t("views:agentTree.act.decide")}</Button>
    if (n.status === "failed" && canAct && u && !at.old && !at.unavailable && u.attempt < w!.max_attempts)
      return <Button size="xs" variant="outline" disabled={at.busy} onClick={() => void at.change("retry", u.id)} data-step-action="retry" data-workflow-retry={u.id}><RotateCcwIcon />{t("views:agentTree.act.retry")}</Button>
    if (open) return <Button size="xs" variant="ghost" className="text-muted-foreground" nativeButton={false} render={<a href={open} />} data-step-action="open">{t("views:agentTree.act.open")}</Button>
    return null
  }
  const attempts: Attempt[] = shown.map((x) => {
    const n = x.node, cost = x.cost?.known ? costOf(x.cost, t) : null, tries = x.row.tries.length
    return {
      id: n.key, name: n.title, approach: [n.bot ? botName(n.bot) : t("views:agentTree.noBot"), model(n.bot)].filter(Boolean).join(" · "),
      icon: <BotAvatar k={n.bot} size={20} />, status: x.look, progress: x.progress, step: x.line, note: x.line,
      figures: [x.row.ms > 0 ? <span title={t("views:agentTree.worked", { time: clockDur(x.row.ms) })}>{longDur(x.row.ms)}</span> : "", cost ? <span title={cost.hint}>{cost.text}</span> : ""],
      action: action(x),
      attrs: { "data-agent-node": n.key, "data-agent-step": n.key, "data-agent-kind": n.kind, "data-agent-status": n.status, "data-agent-look": x.look, "data-agent-tries": tries || undefined, "data-agent-from": n.kind === "fork" ? lanes.parent.get(n.key) : undefined },
    }
  })
  const n = { needs: steps.filter((x) => x.needsYou).length, failed: steps.filter((x) => x.look === "failed").length, running: steps.filter((x) => x.look === "running").length, done: done.length }
  const meta = [tree.bot ? t("views:agentTree.coordinator", { name: botName(tree.bot) }) : null, t("views:agentTree.steps", { count: lanes.steps }),
    range && lanes.worked > 0 ? t("views:agentTree.timeSpan", { wall: longDur(range[1] - range[0]), work: longDur(lanes.worked) }) : null]
  const dot = (tone: string) => <span className={cn("size-1.5 shrink-0 rounded-full", tone)} aria-hidden />
  const failed = steps.filter((x) => x.look === "failed"), asking = steps.filter((x) => x.needsYou)
  const header = <div className="mb-3 flex flex-col gap-1" data-agent-header>
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
      <h2 className="type-label text-tertiary">{t("views:agentTree.title")}</h2>
      {at.old && <span className="rounded-md bg-muted px-1.5 type-label text-muted-foreground" data-agent-old>{t("views:agentTree.earlierRound")}</span>}
      <span className="flex-1" />
      {/* the counts, Away Summary's way: a dot and a number per state; what needs you and what failed open their list */}
      <span className="flex flex-wrap items-center gap-x-1.5 gap-y-1 type-footnote tabular-nums text-muted-foreground" data-agent-counts>
        {n.needs > 0 && <Popover>
          <PopoverTrigger render={<button type="button" className="inline-flex items-center gap-1.5 rounded-md px-1 py-0.5 text-attention hover:bg-attention/10" data-workflow-decisions />}>{dot("bg-attention")}{t("views:agentTree.dots.needs", { count: n.needs })}</PopoverTrigger>
          <PopoverContent align="end" className="w-80 gap-1 rounded-xl p-1.5">
            {asking.map((x) => <a key={x.node.key} href={`#/items/${encodeURIComponent(x.node.item!)}`} className="flex flex-col rounded-lg px-2 py-1.5 type-callout hover:bg-accent">
              <span className="font-medium">{x.node.title}</span>{x.question && <span className="line-clamp-2 type-footnote text-muted-foreground">{x.question}</span>}</a>)}
          </PopoverContent>
        </Popover>}
        {n.failed > 0 && <>{n.needs > 0 && <span aria-hidden>·</span>}<Popover>
          <PopoverTrigger render={<button type="button" className="inline-flex items-center gap-1.5 rounded-md px-1 py-0.5 text-destructive hover:bg-destructive/10" data-agent-failed />}>{dot("bg-destructive")}{t("views:agentTree.dots.failed", { count: n.failed })}</PopoverTrigger>
          <PopoverContent align="end" className="w-80 gap-3 rounded-xl p-3" data-agent-failed-list>
            {failed.map((x) => <div key={x.node.key} className="flex flex-col gap-1.5">
              <div className="flex items-start gap-2 type-callout"><TriangleAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" /><div className="min-w-0"><div className="break-words font-medium">{x.node.title}</div>{x.node.reason && <div className="mt-0.5 break-words type-footnote text-muted-foreground">{x.node.reason}</div>}</div></div>
              <div className="flex justify-end gap-2">
                <Button size="sm" variant="ghost" onClick={() => onSelect(x.node.key)}>{t("views:agentTree.details")}</Button>
                {canAct && x.node.unit && <Button size="sm" disabled={at.busy || at.unavailable || at.old || x.node.unit.attempt >= at.workflow!.max_attempts} onClick={() => void at.change("retry", x.node.unit!.id)} data-workflow-retry={x.node.unit.id}><RotateCcwIcon />{t("views:workflow.retry")}</Button>}
              </div>
            </div>)}
          </PopoverContent>
        </Popover></>}
        {n.running > 0 && <>{(n.needs > 0 || n.failed > 0) && <span aria-hidden>·</span>}<span className="inline-flex items-center gap-1.5 px-1 text-blue" data-agent-running>{dot("bg-blue")}{t("views:agentTree.dots.running", { count: n.running })}</span></>}
        {n.done > 0 && <>{(n.needs > 0 || n.failed > 0 || n.running > 0) && <span aria-hidden>·</span>}<span className="inline-flex items-center gap-1.5 px-1" data-agent-done>{dot("bg-success")}{t("views:agentTree.dots.done", { count: n.done })}</span></>}
      </span>
      <Button size="xs" variant="outline" onClick={() => setGraph(true)} data-agent-graph-open><WorkflowIcon />{t("views:agentTree.graph.open")}</Button>
      {actions}
    </div>
    {/* how long it took against how long its agents worked: the gap is what running at once saved */}
    <span className="type-footnote tabular-nums text-tertiary" data-agent-count>{meta.filter(Boolean).join(" · ")}{c && <span title={c.hint} data-agent-total> · {t("views:agentTree.cost", { usd: c.text })}</span>}</span>
  </div>
  const more = done.length - DONE_SHOWN
  const footer = more > 0 && <button type="button" onClick={() => setAll(!all)} className="mt-2 inline-flex items-center gap-1 rounded-md px-1 py-0.5 type-footnote text-muted-foreground hover:text-foreground" data-agent-more-steps aria-expanded={all}>
    {all ? t("views:agentTree.fewerSteps") : t("views:agentTree.moreSteps", { count: more })}<ChevronDownIcon className={cn("size-3.5 transition-transform duration-180", all && "rotate-180")} /></button>
  return <section ref={box} className="min-w-0" data-agent-team>
    {header}
    <div className="transition-[padding] duration-260 ease-(--ease-out)" style={{ paddingRight: beside }}>
      {attempts.length > 0 ? <ParallelAttempts pickable={false} header={null} attempts={attempts} footer={footer} selected={selected} onSelect={(a) => onSelect(selected === a.id ? null : a.id)} formatCost={fmtUsd}
          labels={stepLabels(t)} />
        : <p className="type-callout text-muted-foreground">{t("views:agentTree.noSteps")}</p>}
    </div>
    {graph && <GraphDialog at={at} onClose={() => setGraph(false)} />}
  </section>
}

/* the full picture (the graph library and the timeline) is a big module: loaded the first time someone opens it */
const CollabGraph = lazy(() => import("@/collab-graph"))
/** "查看全图": the steps as a dependency graph, in a large dialog (a sheet from the bottom on phones) */
function GraphDialog({ at, onClose }: { at: AgentTreeState; onClose: () => void }) {
  const t = useT(), wide = useWide()
  const body = <Suspense fallback={<div className="flex items-center gap-2 p-6 type-callout text-muted-foreground"><Spinner className="size-3.5" />{t("common:state.loading")}</div>}><CollabGraph at={at} /></Suspense>
  if (wide) return <Dialog open onOpenChange={(o) => !o && onClose()}>
    <DialogContent className="flex h-[min(88dvh,960px)] w-[calc(100vw-4rem)] max-w-[1440px]! flex-col gap-3 overflow-hidden rounded-2xl p-5 sm:max-w-[1440px]" data-agent-graph>
      <DialogTitle className="type-headline">{t("views:agentTree.graph.title")}</DialogTitle>
      {body}
    </DialogContent>
  </Dialog>
  return <Sheet open onOpenChange={(o) => !o && onClose()}>
    <SheetContent side="bottom" className="flex h-[92dvh] data-[side=bottom]:h-[92dvh] flex-col gap-3 overflow-hidden rounded-t-2xl p-4 pb-[max(1rem,env(safe-area-inset-bottom))]" data-agent-graph>
      <SheetTitle className="type-headline">{t("views:agentTree.graph.title")}</SheetTitle>
      {body}
    </SheetContent>
  </Sheet>
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
      <summary className="flex cursor-pointer list-none items-center gap-1.5 type-callout"><ChevronRightIcon className="size-3.5 transition-transform duration-180 group-open:rotate-90" />{t("views:workflow.blockers")} · {w.blockers.length}</summary>
      <ul className="mt-1.5 list-disc space-y-1 pl-6 type-callout text-muted-foreground">{w.blockers.map((text, n) => <li key={n} className="break-words">{text}</li>)}</ul>
    </details>}
  </>
}

export function History({ workflow }: { workflow: Workflow }) {
  const t = useT()
  const older = (workflow.history_attempts_total ?? 0) - (workflow.history_attempts?.length ?? 0)
  if (!workflow.history.length && older <= 0) return null
  return <details className="group mt-4" data-workflow-history>
    <summary className="flex cursor-pointer list-none items-center gap-1.5 type-footnote text-muted-foreground hover:text-foreground"><ChevronRightIcon className="size-3.5 transition-transform duration-180 group-open:rotate-90" />{t("views:workflow.history", { count: workflow.history_total ?? workflow.history.length })}</summary>
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
  /* the step's own item (not the page's): its outputs open in the viewer, so its detail loads with the selection */
  const child = node.item && node.item !== at.root ? item : undefined
  useEffect(() => { if (node.item && node.item !== at.root) void loadItem(node.item) }, [node.item, at.root])
  const [view, setView] = useState<OutOpen | null>(null)
  /* a unit's earlier attempts are folded into its row; their outcome shows here */
  const tries = node.kind === "unit" && at.tree ? at.tree.children.filter((c) => c.kind === "attempt" && c.unitId === node.unitId) : []
  const whole = tries.length ? (() => {
    const c = sumCost([...tries, node].map((x) => at.total(x.item))), ms = [...tries, node].reduce((n, x) => n + (workTime(at.stat(x.item)) ?? 0), 0)
    return [c?.known ? costOf(c, t).text : null, ms ? clockDur(ms) : null].filter(Boolean).join(" · ")
  })() : ""
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
      <div className="flex flex-wrap items-center gap-2"><AgentStatusPill s={node.status} />{line.text && <span className="type-footnote tabular-nums text-muted-foreground" data-agent-figures>{tries.length ? t("views:agentTree.thisTry", { x: line.text }) : line.text}</span>}</div>
      {/* a step tried before: what all its tries took together, the figure its card in the full view shows */}
      {tries.length > 0 && whole && <p className="mt-1 type-footnote tabular-nums text-muted-foreground" data-agent-figures-all>{t("views:agentTree.allTries", { count: tries.length + 1, x: whole })}</p>}
      {line.hint && <p className="mt-1 type-footnote text-muted-foreground">{line.hint}</p>}
      <h3 className="mt-3 break-words type-body font-medium">{node.title}</h3>
      {node.kind === "attempt" && <p className="mt-1 type-footnote text-muted-foreground">{t("views:workflow.priorAttempt")}</p>}
      {node.kind === "fork" && item?.parent?.item && <p className="mt-1 type-footnote text-muted-foreground">{t("views:agentTree.forkedFrom", { id: item.parent.item })}</p>}
    </div>
    <div className="flex flex-wrap gap-2">
      {node.item && node.item !== at.root && <Button size="sm" variant="outline" nativeButton={false} render={<a href={`#/items/${encodeURIComponent(node.item)}`} />} data-agent-open-item>{node.kind === "root" ? t("views:agentTree.openItem") : t("views:workflow.openChild")}</Button>}
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
      {stopPending(u) && <p className="text-attention">{t("views:workflow.stopPending")}</p>}
      {st.bots[u.bot_id]?.on === false && <p className="text-attention">{t("views:workflow.botDisabled")}</p>}
    </div>}
    {node.reason && <p className="break-words rounded-lg bg-muted/60 px-3 py-2 type-callout">{node.reason}</p>}
    {tries.length > 0 && <section data-agent-tries><h4 className="mb-1.5 type-label text-tertiary">{t("views:agentTree.earlierTries")}</h4>
      <ul className="flex flex-col gap-1">{tries.map((a) => { const f = figures(at, a, t); return <li key={a.key} className="flex min-w-0 items-center gap-2 type-callout">
        <AgentStatusPill s={a.status} /><span className="shrink-0 tabular-nums">{t("views:agentTree.attempt", { n: a.attempt })}</span>
        <span className="min-w-0 flex-1 truncate type-footnote tabular-nums text-muted-foreground" title={a.reason ?? undefined}>{[f.text, a.reason].filter(Boolean).join(" · ")}</span>
        {a.session && <a href={`#/sessions/${encodeURIComponent(a.session)}`} className="shrink-0 type-footnote text-muted-foreground hover:text-foreground">{t("views:agentTree.openTry")}</a>}</li> })}</ul></section>}
    {u?.summary && u.summary.trim() !== node.reason?.trim() && <section data-agent-summary><h4 className="mb-1.5 type-label text-tertiary">{t("views:agentTree.summary")}</h4><Folded key={node.key + ":summary"}><Markdown text={u.summary} className="break-words type-callout" /></Folded></section>}
    {!!u?.outputs?.length && <section data-workflow-outputs><h4 className="mb-1.5 type-label text-tertiary">{t("views:workflow.outputs")}</h4>
      {u.outputs.map((o, n) => {
        const role = <span className="mr-2 text-muted-foreground">{t(o.role === "record" ? "views:workflow.outputRecord" : o.role === "result" ? "views:workflow.outputResult" : "views:workflow.outputUnspecified")}</span>
        /* what Steve keeps of it: the step's item's output with the same title (or place), which opens in the viewer */
        const kept = child?.outs?.find((x) => x[1] === o.title) ?? (o.location ? child?.outs?.find((x) => x[2] === o.location) : undefined)
        return kept ? <button key={n} type="button" onClick={() => setView({ o: kept })} className="-mx-2 mt-0.5 flex w-[calc(100%+1rem)] min-w-0 items-start gap-1 rounded-lg px-2 py-1 text-left type-callout hover:bg-accent" data-workflow-output={n} data-output-open>
            <span className="min-w-0 flex-1">{role}<span className="break-words">{o.title}</span>{o.location && <span className="block break-all type-code text-muted-foreground">{o.location}</span>}</span><ChevronRightIcon className="mt-0.5 size-4 shrink-0 text-tertiary" /></button>
          : <div key={n} className="mt-1 type-callout" data-workflow-output={n}>{role}<span className="break-words">{o.title}</span>{o.location && <p className="break-all type-code text-muted-foreground">{o.location}</p>}</div>
      })}</section>}
    {child && view && <OutDialog item={child.id} outs={child.outs ?? []} anns={child.anns ?? []} view={view} setView={setView} />}
    {!!u?.checks?.length && <ul className="space-y-1 type-footnote">{u.checks.map((c, n) => <li key={n} className={cn("flex items-start gap-1.5", !c.passed && "text-destructive")}><CircleCheckIcon className="mt-0.5 size-3 shrink-0" /><span>{c.name} · {c.passed ? t("views:workflow.checkPassed") : t("views:workflow.checkFailed")}{c.operated && ` · ${t("views:workflow.operated")}`}</span></li>)}</ul>}
    {u?.goal && <section data-agent-goal><h4 className="mb-1.5 type-label text-tertiary">{t("views:agentTree.goal")}</h4><Folded key={node.key + ":goal"}><Markdown text={u.goal} className="break-words type-callout" /></Folded></section>}
    {node.children.length > 0 && <p className="flex items-center gap-1.5 type-footnote text-muted-foreground"><GitForkIcon className="size-3.5" />{t("views:agentTree.started", { count: node.children.length })}</p>}
    {isOver(node.status) && !line.text && <p className="type-footnote text-muted-foreground">{t("views:agentTree.noFigures")}</p>}
  </div>
}

/** a long text (a step's goal or its report) shown up to a few lines, the rest a click away */
function Folded({ children }: { children: ReactNode }) {
  const t = useT()
  const ref = useRef<HTMLDivElement>(null)
  const [open, setOpen] = useState(false), [long, setLong] = useState(false)
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const fit = () => setLong(el.scrollHeight > el.clientHeight + 4)
    fit()
    const ro = new ResizeObserver(fit); ro.observe(el); if (el.firstElementChild) ro.observe(el.firstElementChild)
    return () => ro.disconnect()
  }, [])
  return <div>
    <div ref={ref} className={cn("relative", !open && "max-h-48 overflow-hidden", !open && long && "[mask-image:linear-gradient(to_bottom,black_70%,transparent)]")}>{children}</div>
    {(long || open) && <button type="button" onClick={() => setOpen(!open)} className="mt-1 inline-flex items-center gap-1 type-footnote text-muted-foreground hover:text-foreground" aria-expanded={open} data-folded-toggle>
      {open ? t("views:agentTree.showLess") : t("views:agentTree.showAll")}<ChevronDownIcon className={cn("size-3.5 transition-transform duration-180", open && "rotate-180")} /></button>}
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
  if (wide) return <aside className="fixed top-16 right-4 bottom-4 z-30 w-[380px] overflow-y-auto rounded-2xl bg-popover p-5 text-popover-foreground shadow-xl motion-safe:animate-in motion-safe:fade-in-0 motion-safe:slide-in-from-right-4 motion-safe:duration-260 motion-safe:ease-(--ease-out)" aria-label={node.title} data-agent-panel>
    <AgentDetail at={at} node={node} onClose={onClose} /></aside>
  return <Sheet open onOpenChange={(open) => !open && onClose()}>
    <SheetContent side="bottom" className="max-h-[85dvh] overflow-y-auto rounded-t-2xl p-4 pb-[max(1rem,env(safe-area-inset-bottom))]" showCloseButton={false} data-agent-panel>
      <SheetTitle className="sr-only">{node.title}</SheetTitle><AgentDetail at={at} node={node} onClose={onClose} /></SheetContent></Sheet>
}

/** while the detail panel is open beside the page, how far the lanes must stay clear of it so that every block can
 * still be picked (on pages a little wider than `lg` the panel covers the main column's right edge, where now is):
 * measured against the panel itself, again whenever either of them changes size */
function usePanelRoom(open: boolean) {
  const ref = useRef<HTMLElement>(null)
  const wide = useWide()
  const [room, setRoom] = useState(0)
  useLayoutEffect(() => {
    if (!open || !wide) { setRoom(0); return }
    const panel = document.querySelector<HTMLElement>("aside[data-agent-panel]")
    const fit = () => {
      // where the panel rests (its layout box: it slides in with a transform), and its gap to the page's edge,
      // kept between it and the lanes too
      const r = ref.current?.getBoundingClientRect(), left = panel?.offsetLeft, gap = panel ? document.documentElement.clientWidth - panel.offsetLeft - panel.offsetWidth : 0
      setRoom(r && left !== undefined ? Math.max(0, Math.ceil(r.right - (left - gap))) : 0)
    }
    fit()
    const ro = new ResizeObserver(fit)
    if (ref.current) ro.observe(ref.current)
    if (panel) ro.observe(panel)
    addEventListener("resize", fit)
    return () => { ro.disconnect(); removeEventListener("resize", fit) }
  }, [open, wide])
  return [ref, room] as const
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
