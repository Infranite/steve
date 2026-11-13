/* ── "查看全图": the whole collaboration of an item, as the steps it was split into ──
 * What it is for: (1) the structure – which steps, which waits for which, what could run at the same time, whether it
 * ended up as one chain; (2) the problems – which step failed, how many tries and why, where the time and money went;
 * (3) the outputs – what each step delivered and what the next one used.
 * So one view: the work graph's steps as a dependency graph. A step is a small card (its Bot, its title, how it ended,
 * its working time and cost); earlier tries are not cards of their own but a stack behind their step. An arrow means
 * "the next step waits for this one", drawn only where no other path already says so (A→C is left out beside
 * A→B→C). Steps are placed by how many steps come before them, so the ones that could run at the same time share a
 * column (a row on a phone) and a chain reads as a line. The coordinator is not a card: the header names it with the
 * totals. A step picked opens its detail beside the graph (under it on a phone): its goal, result, outputs, checks,
 * reasons and tries (AgentDetail, the item page's own), the steps before and after it, and – folded until asked for –
 * its tool calls on a timeline (thread/turn-trace.tsx). Pan by dragging, zoom with the wheel or a pinch. */
import { lazy, Suspense, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react"
import { ReactFlow, Controls, Handle, Position, MarkerType, type Edge, type Node, type NodeProps, type ReactFlowInstance } from "@xyflow/react"
import { ChevronRightIcon } from "lucide-react"
import { Spinner } from "@/components/ui/spinner"
import { useT } from "@/i18n"
import { longDur } from "@/i18n/format"
import { cn } from "@/lib/utils"
import { botName, loadEvents, st } from "@/store"
import { BotAvatar, sessionWork } from "@/ui"
import { directDeps, teamLanes, type AgentNode } from "@/agent-tree"
import { AgentDetail, costOf, stepViews, sumCost, type AgentTreeState, type StepView } from "@/agent-tree-view"
import type { WorkflowUnit } from "@/workflow"

type T = ReturnType<typeof useT>

/* ───────── the graph's shape: pure, so the showroom draws the same with its sample data ───────── */

/** how a step's card reads: seven states, no more */
export type StepState = "done" | "running" | "needsYou" | "failed" | "stopped" | "paused" | "notStarted"
/** one card: everything it shows, already worked out */
export type StepCard = {
  id: string; bot: string | null; botLabel: string; title: string; state: StepState
  /** earlier tries of this step (a stack behind the card) */
  tries: number
  /** the try it passed on, when it passed after earlier ones */
  passedOn?: number
  /** working time and cost, one muted line */
  figures: string; costHint?: string
  /** the steps it waits for directly (no A→C beside A→B→C) */
  depends: string[]
  /** the showroom's cards have no Bot in the store: a letter for the avatar */
  mark?: string
}

const W = 224, H = 116, GAP_LEVEL = 56, GAP_DOWN = 32, GAP_SIBLING = 20

/** every step's level: 0 with nothing to wait for, else one more than the latest step it waits for */
export function levels(cards: StepCard[]): Map<string, number> {
  const by = new Map(cards.map((c) => [c.id, c])), out = new Map<string, number>()
  const level = (id: string, seen: Set<string>): number => {
    if (out.has(id)) return out.get(id)!
    if (seen.has(id)) return 0 // the Hub rejects cycles; an old malformed graph still draws
    seen.add(id)
    const deps = (by.get(id)?.depends ?? []).filter((d) => by.has(d))
    const n = deps.length ? 1 + Math.max(...deps.map((d) => level(d, seen))) : 0
    out.set(id, n)
    return n
  }
  for (const c of cards) level(c.id, new Set())
  return out
}

/** where each card goes: a column per level (a row when `down`), the cards of a level ordered by where the steps they
 * wait for sit, so lines cross as little as they can; every level centred on the same line, so a chain is straight.
 * `per`: a long chain on a wide screen wraps after this many levels into another band below, like a line of text,
 * so its cards stay readable instead of shrinking to fit one line */
export function placeSteps(cards: StepCard[], down: boolean, per = Infinity): { pos: Map<string, { x: number; y: number }>; band: Map<string, number>; size: [number, number] } {
  const lv = levels(cards), order = new Map(cards.map((c, n) => [c.id, n])), by = new Map(cards.map((c) => [c.id, c]))
  const cols: string[][] = []
  for (const c of cards) (cols[lv.get(c.id)!] ??= []).push(c.id)
  const slot = new Map<string, number>()
  cols.forEach((col, n) => {
    if (n > 0) {
      const bary = (id: string) => { const ps = by.get(id)!.depends.filter((d) => slot.has(d)).map((d) => slot.get(d)!); return ps.length ? ps.reduce((a, b) => a + b, 0) / ps.length : 0 }
      col.sort((a, b) => bary(a) - bary(b) || order.get(a)! - order.get(b)!)
    }
    col.forEach((id, i) => slot.set(id, i - (col.length - 1) / 2))
  })
  const pos = new Map<string, { x: number; y: number }>(), band = new Map<string, number>()
  if (down) {
    for (const c of cards) { const l = lv.get(c.id)!; pos.set(c.id, { x: slot.get(c.id)! * (W + GAP_SIBLING) - W / 2, y: l * (H + GAP_DOWN) }); band.set(c.id, 0) }
    const wmax = Math.max(1, ...cols.map((c) => c.length))
    return { pos, band, size: [wmax * (W + GAP_SIBLING) - GAP_SIBLING, cols.length * (H + GAP_DOWN) - GAP_DOWN] }
  }
  /* each band as tall as its tallest column, the bands one under another */
  const bands = Math.ceil(cols.length / per), tall: number[] = []
  for (let r = 0; r < bands; r++) tall.push(Math.max(1, ...cols.slice(r * per, (r + 1) * per).map((c) => c.length)) * (H + GAP_SIBLING) - GAP_SIBLING)
  const top = tall.map((_, r) => tall.slice(0, r).reduce((a, h) => a + h + GAP_LEVEL * 1.5, 0))
  for (const c of cards) {
    const l = lv.get(c.id)!, r = Math.floor(l / per), k = l - r * per
    pos.set(c.id, { x: k * (W + GAP_LEVEL), y: top[r] + tall[r] / 2 + slot.get(c.id)! * (H + GAP_SIBLING) - H / 2 })
    band.set(c.id, r)
  }
  const n = Math.min(per, cols.length)
  return { pos, band, size: [n * (W + GAP_LEVEL) - GAP_LEVEL, top[bands - 1] + tall[bands - 1]] }
}
/** how many levels a band holds in a `w`×`h` box: the fewest bands whose cards come out (nearly) as large as they can */
export function bandWidth(cards: StepCard[], w: number, h: number): number {
  const n = Math.max(1, ...levels(cards).values()) + 1
  let best = n, zoom = 0
  for (let k = 1; k <= Math.min(4, n); k++) {
    const per = Math.ceil(n / k), [sw, sh] = placeSteps(cards, false, per).size
    const z = Math.min(1, (w * 0.88) / sw, (h * 0.88) / sh)
    if (z > zoom + 0.06) { zoom = z; best = per }
  }
  return best
}

/* ───────── the card ───────── */

const TONE: Record<StepState, string> = {
  done: "bg-success/10 text-success", running: "bg-blue/10 text-blue", needsYou: "bg-attention/12 text-attention", failed: "bg-destructive/10 text-destructive",
  stopped: "bg-muted text-muted-foreground", paused: "bg-muted text-muted-foreground", notStarted: "bg-muted text-muted-foreground",
}
export const STEP_DOT: Record<StepState, string> = {
  done: "bg-success", running: "bg-blue dot-running", needsYou: "bg-attention", failed: "bg-destructive", stopped: "bg-tertiary", paused: "bg-tertiary", notStarted: "bg-tertiary",
}

type CardData = { c: StepCard; picked: boolean; dim: boolean; down: boolean }
const RADIUS = "rounded-[calc(var(--radius-xl)-2px)]"
/** a step: its Bot, what it was, how it ended (and on which try), what it took. A step tried before has its earlier
 * tries as cards stacked behind it. The look is the Agent Tree's card (agent-tree-clean): a hairline-lifted card,
 * a 22px avatar tile, a tinted status chip */
function StepNode({ data }: NodeProps<Node<CardData>>) {
  const { c, picked, dim, down } = data, t = useT()
  const tries = c.passedOn ? t("views:agentTree.passedOn", { n: c.passedOn }) : c.tries ? t("views:agentTree.retried", { count: c.tries }) : ""
  const stack = Math.min(2, c.tries)
  return <div className={cn("relative transition-opacity duration-180", dim && "opacity-45")} style={{ width: W, height: H }}>
    {/* the earlier tries, a card or two behind this one */}
    {Array.from({ length: stack }, (_, n) => <span key={n} aria-hidden className={cn("absolute inset-0 bg-card shadow-[var(--lift)] ring-1 ring-border", RADIUS)} data-step-stack
      style={{ transform: `translate(${(stack - n) * 5}px, ${(stack - n) * 5}px)`, opacity: 1 - (stack - n) * 0.2 }} />)}
    <div className={cn("relative flex h-full cursor-pointer flex-col gap-1.5 bg-card px-2.5 pt-2.5 pb-2 shadow-[var(--lift)] transition-shadow duration-180 hover:shadow-[var(--lift-hover)]", RADIUS,
      picked && "ring-2 ring-blue")} data-step-node={c.id} data-step-state={c.state} data-step-tries={c.tries || undefined} data-step-picked={picked || undefined}>
      <Handle type="target" position={down ? Position.Top : Position.Left} className="!size-1 !min-h-0 !min-w-0 !border-0 !bg-transparent" />
      <div className="flex min-w-0 items-center gap-2">
        {c.mark ? <span className="grid size-[22px] shrink-0 place-items-center rounded-md bg-muted type-label text-muted-foreground">{c.mark}</span> : <BotAvatar k={c.bot} size={22} />}
        <span className="min-w-0 flex-1 truncate type-footnote font-medium text-foreground">{c.botLabel}</span>
      </div>
      <p className={cn("line-clamp-2 min-h-8 break-words text-[12px] leading-4", c.state === "notStarted" ? "text-muted-foreground" : "text-foreground/85")} title={c.title}>{c.title}</p>
      <div className="mt-auto flex min-w-0 items-center gap-1.5">
        <span className={cn("inline-flex h-[18px] shrink-0 items-center gap-1 rounded-[4px] pr-1.5 pl-1 text-[11px] leading-4 font-medium whitespace-nowrap", TONE[c.state])} data-step-chip>
          <span className={cn("size-1.5 rounded-full", STEP_DOT[c.state])} />{t(`views:agentTree.graph.state.${c.state}`)}</span>
        {tries && <span className={cn("min-w-0 truncate text-[11px] leading-4 font-medium", c.passedOn ? "text-muted-foreground" : "text-attention")} data-step-tries-label>{tries}</span>}
      </div>
      <div className="min-h-4 truncate text-[11px] leading-4 tabular-nums text-muted-foreground" title={c.costHint} data-step-figures>{c.figures}</div>
      <Handle type="source" position={down ? Position.Bottom : Position.Right} className="!size-1 !min-h-0 !min-w-0 !border-0 !bg-transparent" />
    </div>
  </div>
}
const nodeTypes = { step: StepNode }

/** the graph itself: the cards where `placeSteps` puts them, an arrow per direct dependency – solid once the step
 * before is done, dashed while the next one still waits for it; a step picked lights its arrows and fades the rest */
export function StepGraph({ cards, picked, onPick, down, className }: { cards: StepCard[]; picked: string | null; onPick: (id: string | null) => void; down: boolean; className?: string }) {
  const rf = useRef<ReactFlowInstance<Node<CardData>, Edge> | null>(null)
  const box = useRef<HTMLDivElement>(null)
  /* the box's size, for how the levels wrap into bands */
  const [room, setRoom] = useState<[number, number] | null>(null)
  useLayoutEffect(() => {
    const el = box.current
    if (!el) return
    const fit = () => setRoom((r) => r && Math.abs(r[0] - el.clientWidth) < 40 && Math.abs(r[1] - el.clientHeight) < 40 ? r : [el.clientWidth, el.clientHeight])
    fit()
    const ro = new ResizeObserver(fit); ro.observe(el)
    return () => ro.disconnect()
  }, [])
  const per = useMemo(() => down || !room ? Infinity : bandWidth(cards, room[0], room[1]), [cards, down, room?.[0], room?.[1]])
  const { pos, band } = useMemo(() => placeSteps(cards, down, per), [cards, down, per])
  /* the box grew or shrank (a detail panel opened or closed beside it): the whole graph is framed again */
  const framed = useRef(room)
  useEffect(() => {
    if (!room || framed.current === room) return
    const first = !framed.current
    framed.current = room
    if (!first) void rf.current?.fitView({ padding: 0.12, maxZoom: 1, minZoom: 0.35, duration: 200 })
  }, [room])
  const near = useMemo(() => {
    if (!picked) return null
    const c = cards.find((x) => x.id === picked)
    return new Set([picked, ...(c?.depends ?? []), ...cards.filter((x) => x.depends.includes(picked)).map((x) => x.id)])
  }, [cards, picked])
  const nodes: Node<CardData>[] = cards.map((c) => ({ id: c.id, type: "step", position: pos.get(c.id)!, data: { c, picked: c.id === picked, dim: !!near && !near.has(c.id), down }, draggable: false, width: W, height: H }))
  const state = new Map(cards.map((c) => [c.id, c.state]))
  const edges: Edge[] = cards.flatMap((c) => c.depends.filter((d) => state.has(d)).map((d): Edge => {
    const ready = state.get(d) === "done", hot = d === picked || c.id === picked
    const color = hot ? "var(--blue)" : "color-mix(in srgb, var(--muted-foreground) 60%, transparent)"
    /* an arrow into the next band goes round the cards, not through them */
    return { id: `${d}>${c.id}`, source: d, target: c.id, type: band.get(d) !== band.get(c.id) ? "smoothstep" : "default", style: { stroke: color, strokeWidth: hot ? 1.75 : 1.25, strokeDasharray: ready ? undefined : "4 4", opacity: near && !hot ? 0.35 : 1 },
      markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color } }
  }))
  /* a step picked whose card the panel now covers (or that sits off screen) comes back into view, at the same zoom */
  useEffect(() => {
    const r = rf.current, el = box.current, p = picked ? pos.get(picked) : undefined
    if (!r || !el || !p) return
    const id = setTimeout(() => {
      const { x, y, zoom } = r.getViewport(), w = el.clientWidth, h = el.clientHeight
      const sx = p.x * zoom + x, sy = p.y * zoom + y
      if (sx < 8 || sy < 8 || sx + W * zoom > w - 8 || sy + H * zoom > h - 8) void r.setCenter(p.x + W / 2, p.y + H / 2, { zoom, duration: 260 })
    }, 450)
    return () => clearTimeout(id)
  }, [picked, pos])
  return <div ref={box} className={cn("relative min-h-0 overflow-hidden rounded-xl bg-canvas-graph", className)} data-step-graph={down ? "down" : "right"}
    data-step-edges={edges.map((e) => e.id).join(" ")}>
    {room && <ReactFlow key={(down ? "d:" : "r:") + per + ":" + cards.map((c) => c.id).join()} nodes={nodes} edges={edges} nodeTypes={nodeTypes} onInit={(r) => { rf.current = r }}
      fitView fitViewOptions={{ padding: 0.12, maxZoom: 1, minZoom: 0.35 }} minZoom={0.25} maxZoom={1.6}
      nodesConnectable={false} nodesDraggable={false} elementsSelectable={false} proOptions={{ hideAttribution: true }} panOnScroll={false} zoomOnScroll zoomOnPinch preventScrolling
      onNodeClick={(_, n) => onPick(n.id === picked ? null : n.id)} onPaneClick={() => onPick(null)}>
      <Controls showInteractive={false} position="bottom-right" />
    </ReactFlow>}
  </div>
}

/* ───────── the dialog's content, from an item's collaboration ───────── */

const STATE: Record<string, StepState> = {
  done: "done", running: "running", starting: "running", unconfirmed: "running", failed: "failed", stopped: "stopped", paused: "paused",
}
/** a step of the item page's list as a card */
function cardOf(x: StepView, units: WorkflowUnit[], t: T): StepCard {
  const n = x.node, cost = x.cost?.known ? costOf(x.cost, t) : null, tries = x.row.tries.length
  const state: StepState = x.needsYou ? "needsYou" : STATE[n.status] ?? "notStarted"
  return {
    id: n.key, bot: n.bot, botLabel: n.bot ? botName(n.bot) : t("views:agentTree.noBot"), title: n.title, state, tries,
    passedOn: state === "done" && tries ? n.attempt ?? tries + 1 : undefined,
    figures: [x.row.ms > 0 ? longDur(x.row.ms) : "", cost?.text ?? ""].filter(Boolean).join(" · "), costHint: cost?.hint,
    depends: n.unit ? directDeps(n.unit, units).map((id) => `unit:${id}`) : [],
  }
}

/** re-renders while something runs, so working times move on */
function useNow(live: boolean) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => { if (!live) return; const id = setInterval(() => setNow(Date.now()), 10_000); return () => clearInterval(id) }, [live])
  return now
}
function useMatch(query: string) {
  const [on, setOn] = useState(() => matchMedia(query).matches)
  useEffect(() => { const m = matchMedia(query), f = () => setOn(m.matches); m.addEventListener("change", f); return () => m.removeEventListener("change", f) }, [query])
  return on
}

export default function CollabGraph({ at }: { at: AgentTreeState }) {
  const t = useT()
  const now = useNow(!!at.summary?.active && !at.old)
  const wide = useMatch("(min-width: 1024px)"), down = !useMatch("(min-width: 640px)")
  const [picked, setPicked] = useState<string | null>(null)
  if (!at.tree) return null
  const tree = at.tree
  const lanes = teamLanes(tree, at.stats, now, at.old)
  const steps = stepViews(at, lanes.rows, now, t).filter((x) => x.node.kind === "unit" && x.row.depth === 0)
  const units = at.workflow?.units ?? []
  const cards = steps.map((x) => cardOf(x, units, t))
  const node = picked ? steps.find((x) => x.node.key === picked)?.node ?? null : null
  /* the totals: the coordinator, how many steps, how long it took against how long its agents worked, what it all
   * cost (the coordinator's own turns included), how many tries went again and what failed */
  const total = at.total(tree.item) ?? sumCost(steps.map((x) => x.cost)), c = total?.known ? costOf(total, t) : null
  const retries = steps.reduce((n, x) => n + x.row.tries.length, 0), failed = cards.filter((x) => x.state === "failed").length
  const head = [tree.bot ? t("views:agentTree.coordinator", { name: botName(tree.bot) }) : null, t("views:agentTree.steps", { count: cards.length }),
    lanes.range && lanes.worked > 0 ? t("views:agentTree.timeSpan", { wall: longDur(lanes.range[1] - lanes.range[0]), work: longDur(lanes.worked) }) : null,
    c ? t("views:agentTree.cost", { usd: c.text }) : null].filter(Boolean)
  const detail = node && <StepDetail at={at} node={node} cards={cards} onPick={setPicked} />
  return <div className="flex min-h-0 flex-1 flex-col gap-3" data-collab-graph>
    <div className="flex flex-col gap-0.5" data-collab-head>
      <p className="type-callout tabular-nums text-muted-foreground">
        {at.old && <span className="mr-2 rounded-md bg-muted px-1.5 py-0.5 type-label">{t("views:agentTree.earlierRound")}</span>}
        {head.join(" · ")}
        {retries > 0 && <> · <span className="text-attention" data-collab-retries>{t("views:agentTree.graph.retries", { count: retries })}</span></>}
        {failed > 0 && <> · <span className="text-destructive" data-collab-failed>{t("views:agentTree.graph.failedSteps", { count: failed })}</span></>}
      </p>
      <p className="type-footnote text-tertiary">{t(down ? "views:agentTree.graph.howDown" : "views:agentTree.graph.howRight")}</p>
    </div>
    {cards.length === 0 ? <p className="type-callout text-muted-foreground">{t("views:agentTree.noSteps")}</p>
      : wide ? <div className="flex min-h-0 flex-1 gap-3">
        <StepGraph cards={cards} picked={picked} onPick={setPicked} down={false} className="min-w-0 flex-1" />
        {detail && <aside className="w-[400px] shrink-0 overflow-y-auto rounded-xl bg-card p-5 shadow-[var(--lift)]" aria-label={node!.title} data-collab-detail={node!.key}>{detail}</aside>}
      </div>
      : <div className="flex min-h-0 flex-1 flex-col gap-3">
        <StepGraph cards={cards} picked={picked} onPick={setPicked} down={down} className={detail ? "h-[38%] shrink-0" : "flex-1"} />
        {detail && <div className="min-h-0 flex-1 overflow-y-auto rounded-xl bg-card p-4 shadow-[var(--lift)]" aria-label={node!.title} data-collab-detail={node!.key}>{detail}</div>}
      </div>}
  </div>
}

/** a picked step: the item page's detail (goal, result, outputs, checks, reasons, tries, its actions), then the steps
 * it waited for and the ones that waited for it, and its tool calls, folded */
function StepDetail({ at, node, cards, onPick }: { at: AgentTreeState; node: AgentNode; cards: StepCard[]; onPick: (id: string | null) => void }) {
  const t = useT()
  const me = cards.find((c) => c.id === node.key)
  const before = cards.filter((c) => me?.depends.includes(c.id)), after = cards.filter((c) => c.depends.includes(node.key))
  const link = (c: StepCard) => <button key={c.id} type="button" onClick={() => onPick(c.id)} className="flex w-full min-w-0 items-center gap-2 rounded-lg px-2 py-1.5 text-left type-callout hover:bg-accent" data-collab-link={c.id}>
    <span className={cn("size-1.5 shrink-0 rounded-full", STEP_DOT[c.state])} /><span className="min-w-0 flex-1 truncate">{c.title}</span><span className="shrink-0 type-footnote text-muted-foreground">{c.botLabel}</span></button>
  return <div className="flex min-w-0 flex-col gap-5">
    <AgentDetail at={at} node={node} onClose={() => onPick(null)} />
    {(before.length > 0 || after.length > 0) && <div className="flex flex-col gap-3" data-collab-neighbours>
      {before.length > 0 && <section><h4 className="mb-1 type-label text-tertiary">{t("views:agentTree.graph.before")}</h4><div className="-mx-2">{before.map(link)}</div></section>}
      {after.length > 0 && <section><h4 className="mb-1 type-label text-tertiary">{t("views:agentTree.graph.after")}</h4><div className="-mx-2">{after.map(link)}</div></section>}
    </div>}
    <StepTrace key={node.key} node={node} />
  </div>
}

/* the timeline (Trace Waterfall) is a big component: loaded the first time someone asks for it */
const TurnTrace = lazy(() => import("@/thread/turn-trace"))
/** the sessions a step worked in: its item's, else the one it names */
function sessionsOf(n: AgentNode): string[] {
  const item = n.item ? st.items.find((i) => i.id === n.item) : undefined
  return [...new Set([...(item?.ss ?? []), ...(n.session ? [n.session] : [])])].filter((k) => !!st.S[k])
}
const SHOWN_TURNS = 3
/** a step's tool calls, turn by turn on the session's own timeline; its sessions load when this opens */
function StepTrace({ node }: { node: AgentNode }) {
  const t = useT()
  const [open, setOpen] = useState(false), [all, setAll] = useState(false)
  const ks = sessionsOf(node)
  useEffect(() => { if (open) for (const k of ks) void loadEvents(k) }, [open, ks.join()])
  const loading = ks.some((k) => !st.S[k]?.loaded)
  const turns = open ? ks.flatMap((k) => sessionWork(k).map((w) => ({ k, w }))).sort((a, b) => a.w.t0 - b.w.t0) : []
  const shown = all ? turns : turns.slice(-SHOWN_TURNS)
  const when = (ms: number) => new Date(ms).toLocaleString(undefined, { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })
  return <section data-collab-trace={open ? "open" : "closed"}>
    <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} disabled={!ks.length} className="flex items-center gap-1.5 type-label text-tertiary hover:text-foreground disabled:hover:text-tertiary" data-collab-trace-toggle>
      <ChevronRightIcon className={cn("size-3.5 transition-transform duration-180", open && "rotate-90")} />{t("views:agentTree.graph.trace")}
      {!ks.length && <span className="font-normal">· {t("views:agentTree.graph.noSession")}</span>}
    </button>
    {open && <div className="mt-2 flex flex-col gap-3">
      {loading && <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground" data-collab-trace-loading><Spinner className="size-3" />{t("views:agentTree.graph.loading")}</span>}
      {!loading && !turns.length && <p className="type-footnote text-muted-foreground">{t("views:agentTree.graph.noTurns")}</p>}
      {turns.length > shown.length && <button type="button" onClick={() => setAll(true)} className="self-start type-footnote text-muted-foreground hover:text-foreground">{t("views:agentTree.graph.earlierTurns", { count: turns.length - shown.length })}</button>}
      {shown.map(({ k, w }, n) => <div key={k + ":" + w.t0} className="min-w-0">
        <div className="mb-1 type-footnote tabular-nums text-muted-foreground">{t("views:agentTree.graph.turn", { n: turns.length - shown.length + n + 1 })}{w.t0 ? ` · ${when(w.t0)}` : ""}</div>
        <Suspense fallback={<Spinner className="size-3.5" />}><TurnTrace k={k} evs={w.evs} at={w.at} items={w.items} t0={w.t0} t1={w.t1} live={w.live} cancelled={w.cancelled} /></Suspense>
      </div>)}
    </div>}
  </section>
}
