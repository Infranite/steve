/* ── the item page's collaboration area: an agent tree ──
 * One card per agent: who it is (Bot and model), what it was asked to do, its state, and what it cost and took.
 * The coordinator's direct children sit on a bus under it while they fit side by side; anything deeper, and
 * everything on a narrow page, hangs from a rail under the card that started it. The header line counts the
 * agents, the running ones, failures (a pill opening them with retry) and cards waiting for you, and the total
 * cost. A card selected shows its detail in the page's side column (a sheet on narrow pages). */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { CheckIcon, ChevronDownIcon, ChevronRightIcon, CircleCheckIcon, CircleDashedIcon, CirclePauseIcon, GitForkIcon, HourglassIcon, LoaderCircleIcon, MessageCircleQuestionIcon, MoreHorizontalIcon, NetworkIcon, PauseIcon, PlayIcon, RotateCcwIcon, SquareIcon, TriangleAlertIcon, XIcon } from "lucide-react"
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
import { fmtEst, fmtUsd } from "@/stats/format"
import { botName, loadItem, st } from "@/store"
import { BotAvatar, useNow } from "@/ui"
import { orderedUnits, type Workflow } from "@/workflow"
import { buildAgentTree, elapsed, findNode, isActive, isOver, rollup, summarize, type AgentNode, type AgentStatus, type Cost, type TreeItem, type TreeStat } from "@/agent-tree"

type Action = "pause" | "resume" | "retry" | "cancel" | "single_session"

/** the tree of `root`, its figures, and the work graph's actions (one revision-checked change at a time) */
export function useAgentTree(root: string, workflow?: Workflow | null) {
  const t = useT()
  const tree = buildAgentTree(root, st.items as TreeItem[], workflow)
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
  running: "bg-blue/10 text-blue", unconfirmed: "bg-blue/8 text-blue/80", starting: "bg-blue/8 text-blue/80",
  asking: "bg-attention/12 text-attention", blocked: "bg-attention/12 text-attention",
  done: "bg-success/10 text-success", failed: "bg-destructive/10 text-destructive",
  waiting: "bg-muted text-muted-foreground", pending: "bg-muted text-muted-foreground", paused: "bg-muted text-muted-foreground",
  stopped: "bg-muted text-muted-foreground", proposed: "bg-muted text-muted-foreground",
}
const ICON: Record<AgentStatus, typeof CheckIcon> = {
  running: LoaderCircleIcon, unconfirmed: LoaderCircleIcon, starting: LoaderCircleIcon, asking: MessageCircleQuestionIcon, blocked: TriangleAlertIcon,
  done: CheckIcon, failed: TriangleAlertIcon, waiting: HourglassIcon, pending: CircleDashedIcon, paused: CirclePauseIcon, stopped: SquareIcon, proposed: CircleDashedIcon,
}
export function AgentStatusPill({ s, className }: { s: AgentStatus; className?: string }) {
  const t = useT(), I = ICON[s]
  return <span className={cn("inline-flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-md px-1.5 text-[11.5px] font-medium", TONE[s], className)} data-agent-status={s}>
    <I className={cn("size-3", isActive(s) && "motion-safe:animate-spin")} strokeWidth={2.25} />{t(`views:agentTree.status.${s}`)}</span>
}

/** the estimate, saying so when part of the tokens ran on models without a price; a Hub's own figure when there is no estimate */
const usd = (c: Cost) => c.unpriced ? fmtEst(c.est, c.unpriced) : fmtUsd(c.est || c.reported)
const model = (bot: string | null) => bot && st.bots[bot] ? st.bots[bot].m || st.bots[bot].h : ""

function figures(at: AgentTreeState, n: AgentNode, now: number, t: ReturnType<typeof useT>) {
  const cost = at.total(n.item), time = elapsed(at.stat(n.item), n.status, now)
  return [cost?.known ? t("views:agentTree.cost", { usd: usd(cost) }) : null, time != null ? clockDur(time) : null].filter(Boolean).join(" · ")
}

function Card({ at, n, selected, onSelect, open, toggle }: { at: AgentTreeState; n: AgentNode; selected: boolean; onSelect: () => void; open: boolean; toggle: () => void }) {
  const t = useT(), now = useNow(isActive(n.status))
  const kind = n.unit && n.unit.kind !== "work" ? t(`views:workflow.${n.unit.kind}`) : n.kind === "fork" ? t("views:agentTree.fork") : null
  const line = figures(at, n, now, t)
  return (
    <div role="button" tabIndex={0} aria-pressed={selected} onClick={onSelect} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onSelect() } }}
      className={cn("group/card @container/card relative w-full min-w-0 cursor-pointer rounded-xl border bg-card p-3 text-left shadow-xs transition-[box-shadow,opacity] duration-150 hover:shadow-md focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-blue/30 motion-safe:animate-in motion-safe:fade-in-0",
        selected ? "border-blue ring-3 ring-blue/15" : "border-border", n.kind === "attempt" && "opacity-60 hover:opacity-100", n.status === "pending" && "border-dashed")}
      data-agent-node={n.key} data-agent-kind={n.kind}>
      <div className="flex min-w-0 items-start gap-2">
        <BotAvatar k={n.bot} size={28} className="mt-0.5" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-medium leading-5">{n.bot ? botName(n.bot) : t("views:agentTree.noBot")}</div>
          <div className="truncate text-[12px] leading-4 text-muted-foreground">{model(n.bot) || " "}</div>
        </div>
        <AgentStatusPill s={n.status} className="@max-[200px]/card:hidden" />
      </div>
      <p className="mt-2 line-clamp-2 break-words text-[13px] leading-[18px] text-foreground/90">
        {kind && <span className="mr-1.5 rounded bg-muted px-1 py-px text-[11px] text-muted-foreground">{kind}</span>}{n.title}</p>
      {n.waitingOn.length > 0 && <p className="mt-1.5 truncate text-[12px] text-muted-foreground" data-agent-waiting>{t("views:workflow.waitingFor", { names: n.waitingOn.join(" · ") })}</p>}
      {n.decisions > 0 && <p className="mt-1.5 text-[12px] font-medium text-attention">{t("views:agentTree.asking", { count: n.decisions })}</p>}
      {n.kind === "attempt" && <p className="mt-1.5 text-[12px] text-muted-foreground">{t("views:agentTree.attempt", { n: n.attempt })}</p>}
      <div className={cn("mt-2 min-h-5 items-center gap-2 text-[12px] tabular-nums text-muted-foreground", line || n.children.length > 0 ? "flex" : "hidden @max-[200px]/card:flex")}>
        <AgentStatusPill s={n.status} className="hidden @max-[200px]/card:inline-flex" />
        <span className="min-w-0 flex-1 truncate" data-agent-figures>{line}</span>
        {n.children.length > 0 && <button type="button" onClick={(e) => { e.stopPropagation(); toggle() }} onKeyDown={(e) => e.stopPropagation()} aria-expanded={open}
          aria-label={t(open ? "views:agentTree.collapse" : "views:agentTree.expand", { count: n.children.length })}
          className="-mr-1 inline-flex h-5 items-center gap-0.5 rounded px-1 hover:bg-accent hover:text-foreground" data-agent-toggle={n.key}>
          {n.children.length}{open ? <ChevronDownIcon className="size-3" /> : <ChevronRightIcon className="size-3" />}</button>}
      </div>
    </div>
  )
}

const LINE = "bg-blue/35"
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
    <span aria-hidden className={cn("absolute -top-[3px] -left-[3px] size-[7px] rounded-full border border-blue/60 bg-background")} />
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

const COLUMN = 204
/** the coordinator, then its children on a bus while they fit side by side, otherwise on a rail */
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
  const bus = kids.length > 1 && width >= kids.length * COLUMN
  const props = { at, selected, onSelect, closed, toggle }
  return <div ref={ref} className="min-w-0" data-agent-canvas={bus ? "bus" : "rail"}>
    {bus ? <div className="flex flex-col items-center">
      <div className="w-full max-w-[300px]"><Card at={at} n={root} selected={selected === root.key} onSelect={() => onSelect(root.key)} open={open} toggle={() => toggle(root.key)} /></div>
      <span aria-hidden className={cn("relative h-5 w-px", LINE)}><span className="absolute -top-[3px] -left-[3px] size-[7px] rounded-full border border-blue/60 bg-background" /></span>
      <div className="flex w-full" data-agent-bus>
        {kids.map((c, i) => <div key={c.key} className="relative min-w-0 flex-1 px-1.5 pt-5">
          <span aria-hidden className={cn("absolute top-0 h-px", LINE)} style={{ left: i === 0 ? "50%" : 0, right: i === kids.length - 1 ? "50%" : 0 }} />
          <span aria-hidden className={cn("absolute top-0 left-1/2 h-5 w-px", LINE)} />
          <Branch {...props} n={c} />
        </div>)}
      </div>
    </div> : <Branch {...props} n={root} />}
  </div>
}

/** the header line, notices, the tree and the graph's change history */
export function AgentTree({ at, selected, onSelect }: { at: AgentTreeState; selected: string | null; onSelect: (k: string | null) => void }) {
  const t = useT()
  const { tree, summary, workflow } = at
  if (!tree || !summary) return null
  const total = at.total(tree.item)
  const canAct = !!workflow && !at.single
  return <section className="min-w-0" data-agent-tree data-workflow={workflow ? "" : undefined}>
    <div className="mb-4 flex flex-wrap items-center gap-x-3 gap-y-2" data-agent-header>
      <h2 className="flex items-center gap-2 type-headline"><NetworkIcon className="size-4" />{t("views:agentTree.title")}</h2>
      <span className="flex-1" />
      <span className="type-footnote tabular-nums text-muted-foreground" data-agent-count>{t("views:agentTree.agents", { count: summary.agents })}</span>
      {summary.active > 0 && <span className="type-footnote tabular-nums text-blue">{t("views:agentTree.active", { count: summary.active })}</span>}
      {summary.failed.length > 0 && <Popover>
        <PopoverTrigger render={<button type="button" className="inline-flex items-center gap-1 rounded-md bg-destructive/10 px-1.5 py-0.5 type-footnote font-medium text-destructive hover:bg-destructive/15" data-agent-failed />}>
          {t("views:agentTree.failed", { count: summary.failed.length })}<ChevronDownIcon className="size-3" /></PopoverTrigger>
        <PopoverContent align="end" className="w-80 gap-3 rounded-2xl p-3" data-agent-failed-list>
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
        <PopoverTrigger render={<button type="button" className="inline-flex items-center gap-1 rounded-md bg-attention/12 px-1.5 py-0.5 type-footnote font-medium text-attention hover:bg-attention/18" data-workflow-decisions />}>
          {t("views:agentTree.askingAll", { count: summary.asking.reduce((s, n) => s + n.decisions, 0) })}<ChevronDownIcon className="size-3" /></PopoverTrigger>
        <PopoverContent align="end" className="w-72 gap-1 rounded-2xl p-2">
          {summary.asking.map((n) => <a key={n.key} href={`#/items/${encodeURIComponent(n.item!)}`} className="rounded-lg px-2 py-1.5 type-callout hover:bg-accent">{n.title} · {t("views:workflow.handleDecision")}</a>)}
        </PopoverContent>
      </Popover>}
      {total?.known && <span className="type-footnote tabular-nums text-foreground" data-agent-total>{t("views:agentTree.cost", { usd: usd(total) })}</span>}
      {canAct && <div className="flex items-center gap-1">
        <Button size="sm" variant="outline" disabled={at.busy || workflow!.paused && (at.unavailable || at.old)} onClick={() => void at.change(workflow!.paused ? "resume" : "pause")} data-workflow-toggle>
          {workflow!.paused ? <PlayIcon /> : <PauseIcon />}{workflow!.paused ? t("views:workflow.resume") : t("views:workflow.pause")}</Button>
        {workflow!.paused && <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-agent-more />}><MoreHorizontalIcon /></DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-44"><DropdownMenuItem disabled={at.busy} onClick={at.askSingle} data-workflow-single>{t("views:workflow.single")}</DropdownMenuItem></DropdownMenuContent>
        </DropdownMenu>}
      </div>}
    </div>
    {at.dialogs}
    <Notices at={at} />
    <Canvas at={at} selected={selected} onSelect={(k) => onSelect(k === selected ? null : k)} />
    {workflow && <History workflow={workflow} />}
  </section>
}

function Notice({ mark, children }: { mark?: string; children: ReactNode }) {
  return <p role="status" className="mb-3 break-words rounded-xl border px-3 py-2 type-callout text-muted-foreground" {...(mark ? { [`data-${mark}`]: "" } : {})}>{children}</p>
}
function Notices({ at }: { at: AgentTreeState }) {
  const t = useT(), w = at.workflow
  if (!w) return null
  return <>
    {at.error && <p role="alert" className="mb-3 break-words type-callout text-destructive">{at.error}</p>}
    {at.single && <Notice mark="workflow-single-mode">{t("views:workflow.singleHint")}</Notice>}
    {at.unavailable && !at.single && <Notice mark="workflow-unavailable">{t("views:workflow.unavailable")}</Notice>}
    {at.old && <Notice mark="workflow-prior-cycle">{t("views:workflow.priorCycle")}</Notice>}
    {w.paused && !at.old && !at.single && !at.unavailable && <Notice>{t("views:workflow.pauseHint")}</Notice>}
    {w.blockers.length > 0 && <details className="group mb-3 rounded-xl border px-3 py-2" data-workflow-blockers>
      <summary className="flex cursor-pointer list-none items-center gap-1.5 type-callout"><ChevronRightIcon className="size-3.5 transition-transform group-open:rotate-90" />{t("views:workflow.blockers")} · {w.blockers.length}</summary>
      <ul className="mt-1.5 list-disc space-y-1 pl-6 type-footnote text-muted-foreground">{w.blockers.map((text, n) => <li key={n} className="break-words">{text}</li>)}</ul>
    </details>}
  </>
}

function History({ workflow }: { workflow: Workflow }) {
  const t = useT()
  const older = (workflow.history_attempts_total ?? 0) - (workflow.history_attempts?.length ?? 0)
  if (!workflow.history.length && older <= 0) return null
  return <details className="group mt-4" data-workflow-history>
    <summary className="flex cursor-pointer list-none items-center gap-1.5 type-footnote text-muted-foreground hover:text-foreground"><ChevronRightIcon className="size-3 transition-transform group-open:rotate-90" />{t("views:workflow.history", { count: workflow.history_total ?? workflow.history.length })}</summary>
    {older > 0 && <p className="mt-2 type-footnote text-muted-foreground">{t("views:workflow.historyAttemptsShown", { shown: workflow.history_attempts?.length ?? 0, total: workflow.history_attempts_total })}</p>}
    <ol className="mt-2 space-y-2 border-l pl-3 type-footnote">{workflow.history.slice().reverse().map((entry, n) => <li key={n} className="break-words"><span className="text-muted-foreground">{t("views:workflow.revision", { n: entry.revision })} · </span>{entry.reason}</li>)}</ol>
  </details>
}

/** the selected agent: what it was asked, what it reported, and what can be done with it */
export function AgentDetail({ at, node, onClose }: { at: AgentTreeState; node: AgentNode; onClose: () => void }) {
  const t = useT(), now = useNow(isActive(node.status))
  const w = at.workflow, u = node.kind === "unit" ? node.unit : undefined
  const canAct = !!w && !at.single && !!u
  const running = !!u && (u.status === "running" || u.status === "dispatching")
  const line = figures(at, node, now, t)
  const item = node.item ? st.items.find((i) => i.id === node.item) : undefined
  return <div className="flex min-w-0 flex-col gap-4" data-agent-detail={node.key}>
    <div className="flex items-start gap-2.5">
      <BotAvatar k={node.bot} size={36} />
      <div className="min-w-0 flex-1">
        <div className="truncate type-callout font-medium">{node.bot ? botName(node.bot) : t("views:agentTree.noBot")}</div>
        <div className="truncate type-footnote text-muted-foreground">{model(node.bot)}</div>
      </div>
      <Button variant="ghost" size="icon-sm" className="-mr-1 text-muted-foreground" aria-label={t("common:action.close")} onClick={onClose} data-agent-detail-close><XIcon /></Button>
    </div>
    <div>
      <div className="flex flex-wrap items-center gap-2"><AgentStatusPill s={node.status} />{line && <span className="type-footnote tabular-nums text-muted-foreground">{line}</span>}</div>
      <h3 className="mt-2 break-words type-callout font-medium">{node.title}</h3>
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
    {u && w && <dl className="grid grid-cols-[72px_minmax(0,1fr)] gap-x-3 gap-y-1.5 type-footnote" data-agent-unit={u.id}>
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
    {node.reason && <p className="break-words rounded-lg bg-muted/60 px-3 py-2 type-footnote">{node.reason}</p>}
    {u?.goal && <section><h4 className="mb-1 type-footnote font-medium text-muted-foreground">{t("views:agentTree.goal")}</h4><p className="whitespace-pre-wrap break-words type-footnote">{u.goal}</p></section>}
    {u?.summary && <section><h4 className="mb-1 type-footnote font-medium text-muted-foreground">{t("views:agentTree.summary")}</h4><p className="whitespace-pre-wrap break-words type-footnote">{u.summary}</p></section>}
    {!!u?.outputs?.length && <section data-workflow-outputs><h4 className="mb-1 type-footnote font-medium text-muted-foreground">{t("views:workflow.outputs")}</h4>
      {u.outputs.map((o, n) => <div key={n} className="mt-1 type-footnote"><span className="mr-2 text-muted-foreground">{t(o.role === "record" ? "views:workflow.outputRecord" : o.role === "result" ? "views:workflow.outputResult" : "views:workflow.outputUnspecified")}</span><span>{o.title}</span>{o.location && <p className="break-all font-mono text-muted-foreground">{o.location}</p>}</div>)}</section>}
    {!!u?.checks?.length && <ul className="space-y-1 type-footnote">{u.checks.map((c, n) => <li key={n} className={cn("flex items-start gap-1.5", !c.passed && "text-destructive")}><CircleCheckIcon className="mt-0.5 size-3 shrink-0" /><span>{c.name} · {c.passed ? t("views:workflow.checkPassed") : t("views:workflow.checkFailed")}{c.operated && ` · ${t("views:workflow.operated")}`}</span></li>)}</ul>}
    {node.children.length > 0 && <p className="flex items-center gap-1.5 type-footnote text-muted-foreground"><GitForkIcon className="size-3.5" />{t("views:agentTree.started", { count: node.children.length })}</p>}
    {isOver(node.status) && !line && <p className="type-footnote text-muted-foreground">{t("views:agentTree.noFigures")}</p>}
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
  if (wide) return <aside className="fixed top-16 right-4 bottom-4 z-30 w-[380px] overflow-y-auto rounded-2xl border bg-popover p-4 text-popover-foreground shadow-xl motion-safe:animate-in motion-safe:fade-in-0 motion-safe:slide-in-from-right-4" aria-label={node.title} data-agent-panel>
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
