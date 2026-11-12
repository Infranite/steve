/* Dependency topology and provenance are separate views of the same items. */
import { useMemo, useState } from "react"
import dagre from "@dagrejs/dagre"
import { ReactFlow, Controls, Handle, Position, MarkerType, type Edge, type Node, type NodeProps } from "@xyflow/react"
import { FileTextIcon, TerminalIcon, GitBranchIcon, CircleHelpIcon } from "lucide-react"
import { gRelation, gComps, gNear, ST, SS, st, botName, type GNode, type GEdge } from "@/store"
import { StatusDot } from "@/ui"
import { useT } from "@/i18n"
import { Button } from "@/components/ui/button"

const W = 232, H = 64

function NodeCard({ data }: NodeProps<Node<{ n: GNode; focus: boolean; select: () => void }>>) {
  const { n, focus, select } = data, t = useT()
  const label = n.k === "item" ? ST[n.st]?.[0] : n.k === "sess" ? SS[n.st]?.[0] : n.k === "work" ? t(`shell:graph.workStatus.${n.st}` as any) : ""
  const Icon = n.k === "item" ? FileTextIcon : n.k === "work" ? GitBranchIcon : n.k === "missing" ? CircleHelpIcon : TerminalIcon
  return (
    <div tabIndex={0} role="button" aria-label={n.title} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); select() } }} className={`flex h-[64px] w-[232px] cursor-pointer items-center gap-3 rounded-xl border bg-card px-3 transition-shadow duration-150 hover:shadow-md focus-visible:ring-3 focus-visible:ring-blue/30 ${focus ? "border-blue ring-3 ring-blue/20" : "border-border"}`}>
      <Handle type="target" position={Position.Top} className="!size-1.5 !min-h-0 !min-w-0 !border-0 !bg-border" />
      <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground">
        <Icon className="size-4" strokeWidth={1.5} />
      </span>
      <span className="min-w-0 flex-1">
        <span className="line-clamp-2 break-words text-[13px] font-medium leading-4 text-foreground">{n.k === "item" && <span className="mr-1.5 font-mono text-[11.5px] font-normal text-muted-foreground">{n.id}</span>}{n.title}</span>
        <span className="mt-0.5 flex items-center gap-1.5 truncate text-[12px] text-muted-foreground">
          {n.k === "proj" ? <>{t("shell:graph.projectItems", { count: n.n })}</> : n.k === "missing" ? t("shell:graph.missing") : <><StatusDot s={n.st} />{n.k === "item" ? t("shell:graph.item") : n.k === "work" ? t("shell:graph.work") : n.n ? t("shell:graph.sessionN", { n: n.n }) : t("shell:graph.session")} · {label}{n.b ? " · " + botName(n.b) : ""}{n.arch ? " · " + t("shell:graph.archived") : ""}</>}
        </span>
      </span>
      <Handle type="source" position={Position.Bottom} className="!size-1.5 !min-h-0 !min-w-0 !border-0 !bg-border" />
    </div>
  )
}
const nodeTypes = { card: NodeCard }
type Mode = "blocking" | "lineage"
function layout(ids: string[], E: GEdge[], N: Record<string, GNode>, focus: string | null, select: (id: string) => void) {
  const g = new dagre.graphlib.Graph(); g.setGraph({ rankdir: "TB", nodesep: 28, ranksep: 56, marginx: 16, marginy: 16 }); g.setDefaultEdgeLabel(() => ({}))
  const set = new Set(ids)
  ids.forEach((k) => g.setNode(k, { width: W, height: H }))
  const es = E.filter((e) => set.has(e[0]) && set.has(e[1]))
  es.forEach(([a, b]) => g.setEdge(a, b))
  dagre.layout(g)
  const nodes: Node[] = ids.map((k) => { const p = g.node(k); return { id: k, type: "card", position: { x: p.x - W / 2, y: p.y - H / 2 }, data: { n: N[k], focus: k === focus, select: () => select(k) }, draggable: true } })
  const edges: Edge[] = es.map(([a, b, kind], index) => {
    const dep = kind === "dependency", contains = kind === "contains"
    const color = dep ? "var(--foreground)" : contains ? "var(--border)" : "var(--blue)"
    return { id: a + ">" + b + ":" + kind + ":" + index, source: a, target: b, type: "smoothstep",
      style: { stroke: color, strokeDasharray: kind === "source" ? "5 4" : contains ? "2 5" : undefined, strokeWidth: dep ? 2 : 1.5 },
      markerEnd: contains ? undefined : { type: MarkerType.ArrowClosed, width: 14, height: 14, color } }
  })
  return { nodes, edges }
}

export function RelationGraph({ focus, scope = "comp", height = 420, hideSimple = true, onOpen, minimal = false }: {
  focus?: string | null; scope?: "comp" | "near" | "all"; height?: number; hideSimple?: boolean; onOpen: (n: GNode) => void; minimal?: boolean
}) {
  const t = useT()
  const [mode, setMode] = useState<Mode>(focus && st.S[focus] ? "lineage" : "blocking")
  const [picked, setPicked] = useState<string | null>(focus ?? null)
  const graphFocus = mode === "blocking" && focus && st.S[focus]?.item ? st.S[focus].item : focus
  const { N, E } = gRelation(mode, focus)
  const ids = useMemo(() => {
    if (scope === "near" && graphFocus) return gNear(graphFocus, E).filter((id) => N[id])
    let cs = gComps(N, E)
    if (scope === "comp" && graphFocus) cs = cs.filter((c) => c.includes(graphFocus))
    else { cs = cs.filter((c) => c.some((k) => N[k].k === "item")).sort((a, b) => b.length - a.length); if (hideSimple) cs = cs.filter((c) => c.length > 1) }
    return cs.flat()
  }, [JSON.stringify(E), mode, scope, graphFocus, hideSimple])
  const selected = picked && ids.includes(picked) ? picked : graphFocus && ids.includes(graphFocus) ? graphFocus : null
  const { nodes, edges } = layout(ids, E, N, selected, setPicked)
  const detail = selected ? N[selected] : undefined
  const target = detail?.k === "work" && detail.itemId ? N[detail.itemId] : detail
  const canOpen = target?.k === "item" || target?.k === "sess"
  const relationLabel = (kind: string) => t(`shell:graph.${kind === "dependency" ? "dep" : kind === "source" ? "source" : "contains"}`)
  return (
    <div className="min-w-0 space-y-2" data-graph-mode={mode}>
      <div className="flex flex-wrap gap-2" role="group" aria-label={t("shell:graph.modeLabel")}>
        <Button size="sm" variant={mode === "blocking" ? "default" : "outline"} aria-pressed={mode === "blocking"} onClick={() => { setMode("blocking"); setPicked(focus ?? null) }}>{t("shell:graph.blocking")}</Button>
        <Button size="sm" variant={mode === "lineage" ? "default" : "outline"} aria-pressed={mode === "lineage"} onClick={() => { setMode("lineage"); setPicked(focus ?? null) }}>{t("shell:graph.lineage")}</Button>
      </div>
      <div className="hidden overflow-hidden rounded-xl border bg-canvas-graph sm:block" style={{ height }} data-graph>
      <ReactFlow key={mode + ":" + ids.join()} nodes={nodes} edges={edges} nodeTypes={nodeTypes} fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={0.3} maxZoom={1.6}
        nodesConnectable={false} elementsSelectable={false} proOptions={{ hideAttribution: true }} panOnScroll={false} zoomOnScroll={!minimal} preventScrolling={!minimal}
        onNodeClick={(_, node) => setPicked(node.id)}>
        {!minimal && <Controls showInteractive={false} position="bottom-right" />}
      </ReactFlow>
      </div>
      <div className="space-y-2 sm:hidden" data-graph-list>{ids.map((id) => {
        const n = N[id], incoming = E.filter(([, b]) => b === id).map(([a, , kind]) => `${relationLabel(kind)}: ${N[a]?.title ?? a}`)
        const outgoing = E.filter(([a]) => a === id).map(([, b, kind]) => `${relationLabel(kind)} → ${N[b]?.title ?? b}`)
        const status = n.k === "item" ? ST[n.st]?.[0] : n.k === "sess" ? SS[n.st]?.[0] : n.k === "work" ? t(`shell:graph.workStatus.${n.st}` as any) : t("shell:graph.missing")
        return <button key={id} type="button" onClick={() => setPicked(id)} aria-pressed={selected === id} className="w-full rounded-lg border bg-card p-3 text-left type-footnote" data-graph-list-node={id}>
          <span className="block break-words font-medium">{n.title}</span>
          <span className="mt-1 block break-words text-muted-foreground">{status}{n.b && ` · ${botName(n.b)}`}{n.arch && ` · ${t("shell:graph.archived")}`}</span>
          {incoming.length > 0 && <span className="mt-1 block break-words text-muted-foreground">{incoming.join(" · ")}</span>}
          {outgoing.length > 0 && <span className="mt-1 block break-words text-muted-foreground">{outgoing.join(" · ")}</span>}
        </button>
      })}</div>
      {detail && <div className="min-w-0 rounded-lg border bg-card p-3 type-footnote" data-graph-detail>
        <p className="break-words font-medium">{detail.title}</p>
        <p className="mt-1 break-words text-muted-foreground">{detail.k === "missing" ? t("shell:graph.missing") : detail.k === "work" ? t("shell:graph.work") : detail.k === "sess" ? t("shell:graph.session") : t("shell:graph.item")}{detail.project && ` · ${st.projects.find((p) => p.id === detail.project)?.name ?? detail.project}`}{detail.arch && ` · ${t("shell:graph.archived")}`}</p>
        {mode === "lineage" && detail.sourceAt != null && <p className="mt-1 text-muted-foreground">{t("shell:graph.sourceAt", { n: detail.sourceAt })}</p>}
        {canOpen && <Button size="sm" variant="outline" className="mt-2" onClick={() => onOpen(target)}>{t("shell:graph.open")}</Button>}
      </div>}
    </div>
  )
}

export function GraphLegend() {
  const t = useT()
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 type-footnote text-muted-foreground">
      {[[t("shell:graph.dep"), "var(--foreground)", ""], [t("shell:graph.source"), "var(--blue)", "5 4"], [t("shell:graph.contains"), "var(--border)", "2 5"]].map(([l, c, d]) => (
        <span key={l} className="inline-flex items-center gap-1.5"><svg width="20" height="6"><line x1="0" y1="3" x2="20" y2="3" stroke={c} strokeWidth="1.5" strokeDasharray={d} /></svg>{l}</span>
      ))}
      <span>{t("shell:graph.hint")}</span>
    </div>
  )
}
