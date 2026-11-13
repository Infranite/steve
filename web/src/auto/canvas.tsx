/* The flow canvas: affectUI's Workflow Builder (web/src/components/affect/workflow-builder.tsx) driven by Steve's flow
 * graph. Nodes, ports and what each card says come from the graph and node kinds (nodes.tsx); adding, wiring, removing
 * and the palette edit the graph the same way the inspector does; Run makes a real dry run and the run panel lists the
 * flow's real runs. The graph has no positions: nodes are laid out top to bottom (dagre) and a node you drag keeps its
 * place on this device (localStorage), not in the flow. */
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import dagre from "@dagrejs/dagre"
import { WorkflowBuilderScreen, NODE_H, NODE_W, PLUS_H, TAG_H, type Detail, type WfEdge, type WfKind, type WfNode, type WfPaletteGroup, type WfPort, type WfRun, type WfStep } from "@/components/affect/workflow-builder"
import type { GEdge, GNode, Graph, Run } from "@/api/flow"
import { kindOf, effectful, actionLabel, palette, removeNode, branchLabels, freshId, unreachableNodes, KIND } from "@/auto/nodes"
import { srcIcon, RUN_STATUS } from "@/auto/common"
import { tm } from "@/store"
import { t } from "@/i18n"

/** a Steve node kind's group → the builder's kind (its tag colour and the palette's grouping) */
const GROUP_KIND: Record<string, WfKind> = { trigger: "trigger", logic: "logic", code: "tool", connect: "tool", ai: "agent", wait: "logic", result: "action" }
const wfKind = (k: string): WfKind => GROUP_KIND[(KIND as Record<string, readonly unknown[]>)[k]?.[1] as string] ?? "tool"
/** an edge's label ↔ the port it leaves from ("" is the plain output) */
const portOf = (label: string | undefined) => (label ? "b:" + label : "out")
const labelOf = (port: string) => (port.startsWith("b:") ? port.slice(2) : "")
const edgeId = (e: GEdge) => `${e.from}>${e.to}#${e.label ?? ""}`
const uniq = (xs: string[]) => [...new Set(xs)]
const TRIGGER_TITLE = ["mail", "cal", "im", "github", "webhook", "time", "item", "manual"]
const triggerTitle = (src: string) => t(`auto:triggerTitle.${(TRIGGER_TITLE.includes(src) ? src : "other") as "other"}`)
const clip = (s: unknown, n = 80) => String(s ?? "").replace(/\s+/g, " ").trim().slice(0, n)

/** the outputs a node offers: one per branch (and any label its edges carry), the plain one when it is used or there are no branches */
function portsOf(g: Graph, n: GNode, xOf: (id: string) => number): WfPort[] {
  const outs = g.edges.filter((e) => e.from === n.id), used = uniq(outs.map((e) => e.label ?? ""))
  if (n.kind === "action") return used.map((l) => ({ id: portOf(l), label: l || undefined }))
  const bl = uniq(branchLabels(n).map((b) => String(b ?? "").trim()))
  const named = bl.filter(Boolean)
  const labels = named.length ? [...(used.includes("") || bl.includes("") ? [""] : []), ...named, ...used.filter((l) => l && !named.includes(l))] : uniq(["", ...used])
  // joined outputs sit in the order of the nodes they lead to, so the curves don't cross; free ones keep theirs, after
  const at = (l: string) => { const xs = outs.filter((e) => (e.label ?? "") === l).map((e) => xOf(e.to)); return xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : Infinity }
  return labels.map((l, i) => ({ l, i, x: at(l) })).sort((a, b) => a.x - b.x || a.i - b.i).map(({ l }) => ({ id: portOf(l), label: l || undefined }))
}

/** what a card says: a line in words, then the specifics in mono (or chips); `warn` when something is missing */
function detailOf(n: GNode, flows: { id: string; name: string }[]): Detail {
  const c = n.config ?? {}
  const desc = (k: string) => (k in PALETTE_DESC ? t(PALETTE_DESC[k as keyof typeof PALETTE_DESC]) : "")
  const said = n.note?.trim() || (n.kind === "action" ? desc("action:" + c.action) || actionLabel(c.action) || "" : desc(n.kind))
  const warn = (text: string, mono?: string): Detail => ({ text, mono, warn: true })
  switch (n.kind) {
    case "trigger": {
      const types: string[] = c.types ?? []
      return { text: n.note?.trim() || triggerTitle(c.source), mono: c.source === "time" ? [c.cron, c.tz].filter(Boolean).join(" · ") : types.length ? types.join(t("auto:sep.list")) : c.source === "manual" ? "" : t("auto:builder.allEvents") }
    }
    case "filter": {
      const rules: { label: string; when: string }[] = c.rules ?? []
      if (!rules.length || rules.some((r) => !String(r.when ?? "").trim())) return warn(t("auto:builder.noWhen"), rules.map((r) => r.label).join(" · "))
      return { text: said, mono: rules.map((r) => r.when).join(" · ") }
    }
    case "table": return { text: said, mono: `${(c.inputs ?? []).join(", ")} → ${(c.outputs ?? []).join(", ")} · ${t("auto:builder.rows", { count: (c.rows ?? []).length })}` }
    case "script": { const line = String(c.code ?? "").split("\n").map((l) => l.trim()).find((l) => l && !l.startsWith("export const handler") && l !== "}"); return String(c.code ?? "").trim() ? { text: said, mono: line ?? clip(c.code) } : warn(t("auto:builder.noCode")) }
    case "cmd": return String(c.command ?? "").trim() ? { text: said, mono: c.command } : warn(t("auto:builder.noCommand"))
    case "http": { const url = String(c.url ?? "").trim(); return url && url !== "https://" ? { text: said, mono: `${c.method ?? "GET"} ${url.replace(/^https?:\/\//, "")}` } : warn(t("auto:builder.noUrl"), c.method ?? "GET") }
    case "flow": { const f = flows.find((x) => x.id === c.flow_id); return c.flow_id ? { text: said, mono: f?.name ?? c.flow_id } : warn(t("auto:builder.noFlow")) }
    case "bot": return String(c.instruction ?? "").trim() ? { text: clip(c.instruction), chips: c.branches ?? [] } : { ...warn(t("auto:builder.noInstruction")), chips: c.branches ?? [] }
    case "wait": return { text: said, mono: c.mode === "event" ? [c.match?.source, c.match?.type, c.match?.when].filter(Boolean).join(" · ") : c.mode === "until" ? c.until : c.delay }
    case "approve": return { text: clip(c.question) || said, chips: c.options ?? [] }
    case "action": return { text: said, mono: clip(c.text ?? c.reason ?? (c.action === "item.create" ? (c.mode === "propose" ? t("auto:actionEd.propose") : t("auto:actionEd.start")) : c.action === "item.link" ? c.by : "")) || t("auto:builder.nothing") }
    default: return { text: said, mono: n.kind }
  }
}
/* the palette's line for each kind, which is also a card's line when it has no note */
const PALETTE_DESC = {
  filter: "auto:palette.filter", table: "auto:palette.table", script: "auto:palette.script", cmd: "auto:palette.cmd", http: "auto:palette.http", flow: "auto:palette.flow",
  bot: "auto:palette.bot", wait: "auto:palette.wait", approve: "auto:palette.approve",
  "action:item.create": "auto:palette.itemCreate", "action:item.link": "auto:palette.itemLink", "action:notify": "auto:palette.notify", "action:digest": "auto:palette.digest", "action:ignore": "auto:palette.ignore",
} as const

/** the status chip of a card when no run is shown; `lost`: the trigger doesn't reach it (the Hub won't save that) */
function chipOf(n: GNode, d: Detail, lost: boolean): WfNode["chip"] {
  if (lost) return { text: t("auto:canvas.unlinked"), tone: "warn" }
  if (d.warn) return { text: t("auto:builder.incomplete"), tone: "warn" }
  if (effectful(n)) return { text: t("auto:canvas.needsApproval"), tone: "attn" }
  if (n.kind === "bot") return { text: "AI", tone: "info" }
  // the trigger's event types are already its mono line
  if (n.kind === "trigger") return { text: "" }
  const bl = branchLabels(n).filter(Boolean)
  if (bl.length > 1) return { text: t("auto:builder.branches", { count: bl.length }) }
  if (n.kind === "http") return { text: n.config?.method ?? "GET" }
  return { text: "" }
}

type Pos = Record<string, { x: number; y: number }>
const posKey = (flowId: string) => "steve-flow-pos:" + flowId
const loadPos = (flowId: string): Pos => { try { return JSON.parse(localStorage.getItem(posKey(flowId)) ?? "{}") ?? {} } catch { return {} } }
const savePos = (flowId: string, p: Pos) => { try { if (Object.keys(p).length) localStorage.setItem(posKey(flowId), JSON.stringify(p)); else localStorage.removeItem(posKey(flowId)) } catch { /* private mode: the places last this visit */ } }

/** the graph laid out top to bottom, siblings side by side (the builder's own direction) */
function layout(g: Graph): Pos {
  const d = new dagre.graphlib.Graph()
  d.setGraph({ rankdir: "TB", nodesep: 40, ranksep: TAG_H + PLUS_H + 12, marginx: 0, marginy: 0 }); d.setDefaultEdgeLabel(() => ({}))
  g.nodes.forEach((n) => d.setNode(n.id, { width: NODE_W, height: NODE_H }))
  g.edges.forEach((e) => { if (d.hasNode(e.from) && d.hasNode(e.to)) d.setEdge(e.from, e.to) })
  dagre.layout(d)
  return Object.fromEntries(g.nodes.map((n) => { const p = d.node(n.id); return [n.id, { x: Math.round(p.x - NW2), y: Math.round(p.y - NH2) }] }))
}
const NW2 = NODE_W / 2, NH2 = NODE_H / 2

/** a run's steps as the builder plays them: one per node in the order they ran, each with the edge it came along */
export function stepsOf(g: Graph, run: Run): WfStep[] {
  const out: WfStep[] = []
  let prev: { node: string; branch: string | null } | null = null
  for (const s of run.steps ?? []) {
    const n = g.nodes.find((x) => x.id === s.node_id)
    const e = prev && (g.edges.find((x) => x.from === prev!.node && x.to === s.node_id && (x.label ?? "") === (prev!.branch ?? "")) ?? g.edges.find((x) => x.from === prev!.node && x.to === s.node_id))
    const step: WfStep = {
      node: s.node_id, title: n?.title?.trim() || s.node_id, kind: wfKind(n?.kind ?? ""), ms: s.ms,
      state: s.status === "failed" ? "failed" : s.status === "waiting" ? "waiting" : "done", via: e ? edgeId(e) : undefined, error: s.error ?? undefined,
      note: s.error ? "" : s.branch ? "→ " + s.branch : s.output?.skipped ? String(s.output.skipped) : s.status === "waiting" ? t("auto:runStatus.waiting") : clip(JSON.stringify(s.output ?? null), 100),
      attrs: { "data-step": s.node_id },
    }
    if (out.length && out[out.length - 1].node === s.node_id) out[out.length - 1] = { ...step, via: out[out.length - 1].via }
    else out.push(step)
    prev = { node: s.node_id, branch: s.branch }
  }
  return out
}

const RUN_STATE: Record<string, WfRun["state"]> = { run: "running", wait: "waiting", done: "done", fail: "failed", idle: "idle" }
/** a run as the run panel lists it; with its steps when it is the one played */
function wfRun(r: Run, steps?: WfStep[], extra?: ReactNode): WfRun {
  const [label, dot] = RUN_STATUS[r.status] ?? [r.status, "idle"]
  return {
    id: r.id, title: r.outcome_label || label, state: RUN_STATE[dot] ?? "idle", error: r.error, steps, extra,
    when: [tm(r.started), r.dry_run ? t("auto:rule.dryRun") : "", "v" + r.flow_version].filter(Boolean).join(" · "),
    payload: r.steps?.[0]?.input, attrs: { "data-run-row": r.id },
  }
}

export function FlowCanvas({ g, flowId, flows, sel, onSel, onChange, renderSettings, runs, played, onDry, onOpenRun, onCloseRun, runExtra, busy, className, height }: {
  g: Graph; flowId: string; flows: { id: string; name: string }[]
  sel: string | null; onSel: (id: string | null) => void
  /** a new graph; `sel`: the node to select (a node just added) */
  onChange: (g: Graph, sel?: string) => void
  renderSettings: (id: string) => ReactNode
  /** the flow's recent runs, newest first */
  runs: Run[]
  /** the run shown on the canvas (a dry run or a past one); a new key plays it again */
  played: { key: string; run: Run } | null
  onDry: () => void; onOpenRun: (id: string) => void; onCloseRun: () => void
  /** what a running run's row offers (cancel) */
  runExtra?: (r: Run) => ReactNode
  busy?: boolean; className?: string; height?: string
}) {
  const [moved, setMoved] = useState<Pos>(() => loadPos(flowId))
  /* where every node stood before an edit made on the canvas: adding, wiring or removing doesn't move the others (a
   * new layout would); a node that arrives some other way (Steve's edit, a rollback) lays the whole flow out again */
  const [held, setHeld] = useState<Pos>({})
  /* the graphs before each edit made on the canvas (add, wire, unwire, remove), for ⌘Z; an edit from elsewhere ends it */
  const history = useRef<{ before: Graph; after: string; pos: Pos }[]>([])
  const shape = JSON.stringify([g.nodes.map((n) => n.id), g.edges.map((e) => [e.from, e.to])])
  const auto = useMemo(() => layout(g), [shape])
  const keep = g.nodes.every((n) => moved[n.id] || held[n.id])
  /* a flow laid out again (Steve's edit, a rollback) starts clean: places dragged for the old shape would sit on the new one */
  const lastShape = useRef(shape)
  useEffect(() => {
    if (shape === lastShape.current) return
    lastShape.current = shape
    if (!keep && Object.keys(moved).length) { setMoved({}); savePos(flowId, {}) }
  }, [shape])
  const lost = useMemo(() => new Set(unreachableNodes(g)), [g])
  const posOf = (id: string) => moved[id] ?? (keep ? held[id] : undefined) ?? auto[id]
  const nodes: WfNode[] = useMemo(() => g.nodes.map((n) => {
    const [label, I] = kindOf(n.kind), d = detailOf(n, flows), p = posOf(n.id) ?? { x: 0, y: 0 }
    const xOf = (id: string) => posOf(id)?.x ?? 0
    return {
      id: n.id, kind: wfKind(n.kind), tag: n.kind === "action" ? actionLabel(n.config?.action) ?? label : label, title: n.title?.trim() || label,
      icon: n.kind === "trigger" ? srcIcon(n.config?.source) : I, x: p.x, y: p.y, input: n.kind !== "trigger", ports: portsOf(g, n, xOf), detail: d, chip: chipOf(n, d, lost.has(n.id)),
      removable: n.kind !== "trigger", attrs: { "data-rule-node": n.id },
    }
  }), [g, moved, held, auto, flows, lost])
  /** where every node stands now */
  const here = (): Pos => Object.fromEntries(nodes.map((n) => [n.id, { x: n.x, y: n.y }]))
  const edges: WfEdge[] = useMemo(() => g.edges.filter((e) => g.nodes.some((n) => n.id === e.from) && g.nodes.some((n) => n.id === e.to)).map((e) => ({ id: edgeId(e), from: e.from, to: e.to, port: portOf(e.label), label: e.label || undefined })), [g])
  const groups: WfPaletteGroup[] = useMemo(() => palette().map((grp, gi) => ({
    id: "g" + gi, label: grp.g,
    items: grp.items.map(([k, title, desc, cfg]) => ({ type: k === "action" ? "action:" + cfg.action : k, label: title, kind: wfKind(k), icon: kindOf(k)[1], description: desc })),
  })), [])
  const steps = useMemo(() => (played ? stepsOf(g, played.run) : []), [played?.key, g])
  const list: WfRun[] = useMemo(() => {
    const xs = played && !runs.some((r) => r.id === played.run.id) ? [played.run, ...runs] : runs
    return xs.map((r) => wfRun(r, played?.run.id === r.id ? steps : undefined, runExtra?.(r)))
  }, [runs, played?.key, steps])
  const play = played ? { key: played.key, run: wfRun(played.run, steps) } : null

  /** a dragged place is kept for the nodes the flow has (a removed node's id may come back as a new node) */
  const keepPlaces = (p: Pos) => { const next = Object.fromEntries(Object.entries(p).filter(([k]) => g.nodes.some((n) => n.id === k))); savePos(flowId, next); return next }
  const move = (id: string, x: number, y: number) => setMoved((m) => keepPlaces({ ...m, [id]: { x, y } }))
  /** an edit on the canvas: the other nodes stay where they are, and ⌘Z can take it back */
  const edit = (next: Graph, select?: string, at: Pos = {}) => {
    const pos = here()
    setHeld({ ...pos, ...at })
    // the stack only goes on while each edit is the one before (an edit from elsewhere breaks it)
    const past = history.current.at(-1)?.after === JSON.stringify(g) ? history.current : []
    history.current = [...past.slice(-29), { before: g, after: JSON.stringify(next), pos }]
    onChange(next, select)
  }
  const add = (type: string, at: { x: number; y: number }, from?: { node: string; port: string }) => {
    const item = palette().flatMap((p) => p.items).find(([k, , , cfg]) => (k === "action" ? "action:" + cfg.action : k) === type)
    if (!item) return undefined
    const [kind, title, , cfg] = item
    const id = freshId(g, kind)
    if (moved[id]) setMoved((m) => { const { [id]: _, ...rest } = m; return keepPlaces(rest) })
    edit({ nodes: [...g.nodes, { id, kind, title, note: "", config: JSON.parse(JSON.stringify(cfg)) }], edges: from ? [...g.edges, { from: from.node, to: id, label: labelOf(from.port) }] : g.edges }, id, { [id]: at })
    return id
  }
  const remove = (id: string) => {
    if (moved[id]) setMoved((m) => { const { [id]: _, ...rest } = m; return keepPlaces(rest) })
    edit(removeNode(g, id))
  }
  const undo = () => {
    const last = history.current.at(-1)
    if (!last || last.after !== JSON.stringify(g)) { history.current = []; return false }
    history.current = history.current.slice(0, -1); setHeld({ ...here(), ...last.pos }); onChange(last.before); return true
  }

  return (
    <div className={className} style={{ height }} data-rule-canvas>
      <WorkflowBuilderScreen className="rounded-xl shadow-[inset_0_0_0_1px_var(--border)]" nodes={nodes} edges={edges} palette={groups}
        settings={sel} onSettings={onSel} renderSettings={renderSettings}
        onAdd={add} onMove={move} onRemove={remove} onUndo={undo}
        onConnect={(from, port, to) => edit({ ...g, edges: [...g.edges, { from, to, label: labelOf(port) }] })}
        onDisconnect={(eid) => edit({ ...g, edges: g.edges.filter((e) => edgeId(e) !== eid) })}
        runs={list} play={play} onRun={onDry} onOpenRun={onOpenRun} onCloseRun={onCloseRun} busy={busy} />
    </div>
  )
}
