/* ── "查看全图": the whole collaboration of an item, two ways ──
 * 调用树 (affectUI Run Inspector): the coordinator is the root, every step, earlier try and fork an agent under it,
 * and under each agent its own calls – thinking, notes and tool calls from its sessions' events, the way a turn's
 * timeline draws them (thread/turn-trace.tsx) – with its cost, tokens and working time from the Hub's figures
 * (GET /stats/tree). The sessions load when this opens, not before.
 * 交接 (affectUI Agent Handoff): the steps in the order of the work, each with the agent that ran it; under each,
 * what it received – every step it depends on, with the outputs that step reported (files, results, records).
 * Nothing here is invented: no lost items, no dragging work to another agent. */
import { useEffect, useMemo, useState } from "react"
import { ArrowUpRightIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { RunInspector, type RunInspectorLabels, type RunSpan, type RunSpanStatus } from "@/components/affect/run-inspector-clean"
import { AgentHandoff, type Handoff, type HandoffAgent, type HandoffItemKind, type HandoffLabels, type HandoffTask } from "@/components/affect/agent-handoff-clean"
import { t as t0, useT } from "@/i18n"
import { clockDur, longDur } from "@/i18n/format"
import { fmtUsd } from "@/stats/format"
import { botName, loadEvents, st } from "@/store"
import { BotAvatar, Segmented, sessionWork } from "@/ui"
import { turnSpans } from "@/thread/turn-trace"
import { orderedUnits, type WorkflowUnit } from "@/workflow"
import { isActive, isOver, walk, workTime, type AgentNode, type AgentStatus } from "@/agent-tree"
import { model, type AgentTreeState } from "@/agent-tree-view"

type T = ReturnType<typeof useT>
const msOf = (x: number) => (x < 1e12 ? x * 1000 : x)
const SPAN: Record<AgentStatus, RunSpanStatus> = {
  done: "done", failed: "failed", stopped: "stopped", running: "running", starting: "running", unconfirmed: "running", asking: "waiting",
  idle: "idle", paused: "paused", proposed: "queued", pending: "queued", waiting: "queued", blocked: "queued",
}

/** how long a call or an agent took: tenths of a second while it is short */
const took = (ms: number) => ms < 50 ? "<" + t0("common:dur.seconds", { count: 0.1 }) : ms < 10_000 ? t0("common:dur.seconds", { count: Math.round(ms / 100) / 10 }) : longDur(ms)

/** the sessions an agent of the tree worked in: its item's, else the one it names */
function sessionsOf(n: AgentNode): string[] {
  const item = n.item ? st.items.find((i) => i.id === n.item) : undefined
  return [...new Set([...(item?.ss ?? []), ...(n.session ? [n.session] : [])])].filter((k) => !!st.S[k])
}

/** the tree as Run Inspector spans: an agent per node, its own calls under it, times in ms from `base` */
function runOf(at: AgentTreeState, root: AgentNode, base: number, t: T): RunSpan {
  const agent = (n: AgentNode): RunSpan => {
    const s = at.stat(n.item)
    const calls: RunSpan[] = sessionsOf(n).flatMap((k) => sessionWork(k).flatMap((w, j) => turnSpans(w.evs, w.at, w.items, base, w.t1, w.live, t).map((c): RunSpan => ({
      id: `${n.key}:${k}:${j}:${c.id}`, kind: c.kind === "llm" ? "llm" : "tool", name: c.name, detail: c.detail, tool: c.kind === "llm" ? undefined : c.name,
      status: c.status === "running" ? "running" : c.status === "failed" ? "failed" : "done", start: c.start, end: c.end,
      input: c.input, output: c.output, error: c.error,
    }))))
    const first = s?.first_turn ? msOf(s.first_turn) - base : calls.length ? Math.min(...calls.map((c) => c.start)) : 0
    const closed = s?.done_at ? msOf(s.done_at) - base : s?.dropped_at ? msOf(s.dropped_at) - base : null
    const lastCall = calls.length ? Math.max(...calls.map((c) => c.end ?? c.start)) : null
    const status: RunSpanStatus = n.decisions > 0 && !isOver(n.status) ? "waiting" : SPAN[n.status]
    const end = isActive(n.status) || status === "waiting" ? undefined : closed ?? (lastCall !== null ? Math.max(first, lastCall) : s?.agent_ms ? first + s.agent_ms : first)
    const item = n.item ? st.items.find((i) => i.id === n.item) : undefined
    const worked = workTime(s)
    return {
      id: n.key, kind: "agent", name: n.bot ? botName(n.bot) : t("views:agentTree.noBot"), model: model(n.bot) || undefined, task: n.title, icon: <BotAvatar k={n.bot} size={20} />,
      status, start: Math.max(0, first), end, cost: s?.est || undefined, tokensIn: s?.tokens || undefined, time: worked ? clockDur(worked) : "",
      ask: status === "waiting" ? item?.waits[0]?.q : undefined, error: n.status === "failed" ? n.reason ?? undefined : undefined,
      children: [...calls.sort((a, b) => a.start - b.start), ...n.children.map(agent)],
    }
  }
  return agent(root)
}

/** where the run starts: the first turn any of its agents took, or the first call any of them made */
function startOf(at: AgentTreeState, root: AgentNode): number {
  const xs: number[] = []
  walk(root, (n) => {
    const s = at.stat(n.item)
    if (s?.first_turn) xs.push(msOf(s.first_turn))
    for (const k of sessionsOf(n)) { const ts = st.S[k]?.ts ?? []; const f = ts.find(Boolean); if (f) xs.push(f) }
  })
  return xs.length ? Math.min(...xs) : Date.now()
}

/** an output as a thing handed on: a record as a note, a file (by its kind or a path), a result as a fact (a
 * decision when its kind says so); the Bot names the kind freely, so anything else is a note */
function kindOf(o: NonNullable<WorkflowUnit["outputs"]>[number]): HandoffItemKind {
  if (o.role === "record") return "note"
  if (/file|code|diff|patch|doc|image|screenshot|pdf/i.test(o.kind) || (o.location && !/^https?:/.test(o.location))) return "file"
  if (o.role === "result") return /decision/i.test(o.kind) ? "decision" : "fact"
  return "note"
}

const tr = (t: T) => t as unknown as (k: string, o?: object) => string
/** Agent Handoff's words, translated */
export function handoffLabels(t: T): Partial<HandoffLabels> {
  const L = (k: string, o?: Record<string, unknown>) => tr(t)(`views:agentTree.graph.handoffs.${k}`, o)
  return { none: L("none"), idle: L("idle"), working: L("working"), ready: L("ready"), done: L("done"), warning: L("warning"), upNext: L("upNext"), nothing: L("nothing"), allLost: L("allLost"), empty: L("empty"),
    handingOn: L("handingOn"), close: L("close"), tools: L("tools"), lost: L("lost"), restore: L("restore"),
    more: (n) => L("more", { count: Number(n) }), costSoFar: (usd) => L("costSoFar", { usd }), with: (name) => L("with", { name }), received: (name) => L("received", { name }), from: (name) => L("from", { name }) }
}
/** Run Inspector's words (and its Agent Tree's), translated */
export function inspectorLabels(t: T): Partial<RunInspectorLabels> {
  const I = (k: string, o?: Record<string, unknown>) => tr(t)(`views:agentTree.graph.inspector.${k}`, o)
  return { whole: I("whole"), title: t("views:agentTree.title"), less: I("less"), tool: I("tool"), model: I("model"), stillRunning: I("stillRunning"), noOutput: I("noOutput"), calledWith: I("calledWith"), prompt: I("prompt"),
    callsOf: (name) => I("callsOf", { name }), more: (n) => I("more", { count: n }),
    facts: { duration: I("duration"), cost: I("cost"), tokens: I("tokens"), modelCalls: I("modelCalls"), toolCalls: I("toolCalls"), errors: I("errors") },
    state: { failed: I("failed"), running: I("running"), done: I("done"), queued: I("queued") },
    tree: { status: { queued: I("status.queued"), running: I("status.running"), waiting: I("status.waiting"), done: I("status.done"), failed: I("status.failed"), stopped: I("status.stopped"), idle: t("views:agentTree.status.idle"), paused: t("views:agentTree.status.paused") },
      agents: (n) => I("agents", { count: n }), running: (n) => I("running", { count: n }), needsYou: (n) => I("needsYou", { count: n }), failed: (n) => I("failed", { count: n }),
      needsYouList: I("needsYouList"), failedList: I("failedList"), branch: I("branch"), showSubs: (n, name) => I("showSubs", { count: n, name }), foldSubs: (name) => I("foldSubs", { name }), beside: (name) => I("beside", { name }) } }
}

function Handoffs({ at, t }: { at: AgentTreeState; t: T }) {
  const w = at.workflow, tree = at.tree
  const [wide, setWide] = useState(() => matchMedia("(min-width: 1024px)").matches)
  useEffect(() => { const m = matchMedia("(min-width: 1024px)"), on = () => setWide(m.matches); m.addEventListener("change", on); return () => m.removeEventListener("change", on) }, [])
  if (!w || !tree) return <p className="p-4 type-callout text-muted-foreground">{t("views:agentTree.graph.handoffs.none")}</p>
  const units = orderedUnits(w.units)
  const node = (u: WorkflowUnit) => tree.children.find((c) => c.key === `unit:${u.id}`)
  const costOfUnit = (u: WorkflowUnit) => {
    const ns = tree.children.filter((c) => c.unitId === u.id)
    const xs = ns.map((x) => at.total(x.item)).filter((c) => c?.known)
    return xs.length ? xs.reduce((s, c) => s + c!.est, 0) : undefined
  }
  const agents: HandoffAgent[] = units.map((u) => ({ id: u.id, name: botName(u.bot_id), model: model(u.bot_id) || undefined, avatar: <BotAvatar k={u.bot_id} size={18} />, description: u.title, cost: costOfUnit(u) }))
  const tasks: HandoffTask[] = units.map((u) => {
    const n = node(u), s = n?.status ?? "pending"
    const status: HandoffTask["status"] = s === "done" ? "done" : s === "failed" ? "warning" : isActive(s) ? "working" : "todo"
    const item = n?.item ? st.items.find((i) => i.id === n.item) : undefined
    const note = status === "working" ? item?.now || undefined : status === "warning" ? n?.reason ?? undefined : status === "done" && u.attempt > 1 ? t("views:agentTree.passedOn", { n: u.attempt }) : undefined
    return { id: u.id, title: u.title, description: u.goal, status, agent: u.id, note, logo: <BotAvatar k={u.bot_id} size={14} /> }
  })
  const handoffs: Handoff[] = units.flatMap((u) => u.depends_on.map((d) => units.find((x) => x.id === d)).filter((x): x is WorkflowUnit => !!x).map((d) => ({
    id: `${d.id}->${u.id}`, from: d.id, to: u.id, task: d.title,
    items: (d.outputs ?? []).map((o, k) => ({ id: `${d.id}:${k}`, label: o.title, kind: kindOf(o) })),
  })))
  return <div className="min-h-0 flex-1 overflow-auto p-1" data-agent-handoff>
    <div style={{ minWidth: wide ? Math.max(0, agents.length * 180) : undefined }}>
      <AgentHandoff className="steve-handoff" agents={agents} tasks={tasks} handoffs={handoffs} inbound
        labels={handoffLabels(t)} />
    </div>
  </div>
}

function CallTree({ at, t, onSelect }: { at: AgentTreeState; t: T; onSelect: (k: string) => void }) {
  const tree = at.tree!
  const ks = useMemo(() => { const out: string[] = []; walk(tree, (n) => out.push(...sessionsOf(n))); return [...new Set(out)] }, [tree])
  useEffect(() => { for (const k of ks) void loadEvents(k) }, [ks.join()])
  const loading = ks.filter((k) => !st.S[k]?.loaded).length
  const [picked, setPicked] = useState<string | null>(null)
  const base = startOf(at, tree)
  const run = runOf(at, tree, base, t)
  const pickedNode = picked ? (() => { let hit: AgentNode | null = null; walk(tree, (n) => { if (n.key === picked) hit = n }); return hit as AgentNode | null })() : null
  return <div className="flex min-h-0 flex-1 flex-col gap-2">
    <div className="flex min-h-7 flex-wrap items-center gap-2 type-footnote text-muted-foreground">
      {loading > 0 && <span className="inline-flex items-center gap-1.5" data-agent-graph-loading><Spinner className="size-3" />{t("views:agentTree.graph.loading")}</span>}
      <span className="flex-1" />
      {pickedNode && pickedNode.kind !== "root" && <Button size="xs" variant="outline" onClick={() => onSelect(pickedNode.kind === "attempt" ? tree.children.find((c) => c.kind === "unit" && c.unitId === pickedNode.unitId)?.key ?? pickedNode.key : pickedNode.key)} data-agent-graph-detail>
        {pickedNode.title}<ArrowUpRightIcon /></Button>}
    </div>
    <div className="mx-auto min-h-0 w-full max-w-[1040px] flex-1 overflow-auto p-1" data-agent-inspector>
      <RunInspector run={run} startedAt={base} selected={picked} onSelectedChange={setPicked} formatCost={fmtUsd} formatTook={took}
        labels={inspectorLabels(t)} />
    </div>
  </div>
}

export default function CollabGraph({ at, onSelect }: { at: AgentTreeState; onSelect: (k: string) => void }) {
  const t = useT()
  const [tab, setTab] = useState<"tree" | "handoff">("tree")
  if (!at.tree) return null
  return <div className="flex min-h-0 flex-1 flex-col gap-3">
    <Segmented size="sm" value={tab} onChange={setTab} label={t("views:agentTree.graph.title")} options={[["tree", t("views:agentTree.graph.tree")], ["handoff", t("views:agentTree.graph.handoff")]] as const} />
    {tab === "tree" ? <CallTree at={at} t={t} onSelect={onSelect} /> : <Handoffs at={at} t={t} />}
  </div>
}
