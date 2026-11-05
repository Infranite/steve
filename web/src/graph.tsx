/* Relation graph — React Flow + dagre layout, styled per DESIGN.md:
 * white cards with a hairline, grey edges, dashed hand-offs, blue forks, no dot grid. */
import { useMemo } from "react"
import dagre from "@dagrejs/dagre"
import { ReactFlow, Controls, Handle, Position, MarkerType, type Edge, type Node, type NodeProps } from "@xyflow/react"
import { FileTextIcon, FolderIcon, TerminalIcon } from "lucide-react"
import { gBuild, gComps, gNear, gProjects, ST, SS, botName, type GNode } from "@/store"
import { StatusDot } from "@/ui"
import { t, useT } from "@/i18n"

const W = 232, H = 64

function NodeCard({ data }: NodeProps<Node<{ n: GNode; focus: boolean }>>) {
  const { n, focus } = data, t = useT()
  const label = n.k === "item" ? ST[n.st]?.[0] : SS[n.st]?.[0]
  const Icon = n.k === "item" ? FileTextIcon : n.k === "proj" ? FolderIcon : TerminalIcon
  return (
    <div className={`flex h-[64px] w-[232px] cursor-pointer items-center gap-3 rounded-xl border bg-card px-3 transition-shadow duration-150 hover:shadow-md ${focus ? "border-blue ring-3 ring-blue/20" : "border-border"}`}>
      <Handle type="target" position={Position.Top} className="!size-1.5 !min-h-0 !min-w-0 !border-0 !bg-border" />
      <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground">
        <Icon className="size-4" strokeWidth={1.5} />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[13px] font-medium text-foreground">{n.k === "item" && <span className="mr-1.5 font-mono text-[11.5px] font-normal text-muted-foreground">{n.id}</span>}{n.title}</span>
        <span className="mt-0.5 flex items-center gap-1.5 truncate text-[12px] text-muted-foreground">
          {n.k === "proj" ? <>{t("shell:graph.projectItems", { count: n.n })}</> : <><StatusDot s={n.st} />{n.k === "item" ? t("shell:graph.item") : n.n ? t("shell:graph.sessionN", { n: n.n }) : t("shell:graph.session")} · {label}{n.b ? " · " + botName(n.b) : ""}</>}
        </span>
      </span>
      <Handle type="source" position={Position.Bottom} className="!size-1.5 !min-h-0 !min-w-0 !border-0 !bg-border" />
    </div>
  )
}
const nodeTypes = { card: NodeCard }
/* the store's edge kinds ("派生", "交接", "依赖", "项目"; "" is a run) and the words shown for them */
const edgeLabel = (l: string) => l.startsWith("派生") ? t("shell:graph.fork") : l === "交接" ? t("shell:graph.handoff") : l === "依赖" ? t("shell:graph.dep") : l === "项目" ? t("shell:graph.project") : l

function layout(ids: string[], E: [string, string, string][], N: Record<string, GNode>, focus?: string | null) {
  const g = new dagre.graphlib.Graph(); g.setGraph({ rankdir: "TB", nodesep: 28, ranksep: 56, marginx: 16, marginy: 16 }); g.setDefaultEdgeLabel(() => ({}))
  const set = new Set(ids)
  ids.forEach((k) => g.setNode(k, { width: W, height: H }))
  const es = E.filter((e) => set.has(e[0]) && set.has(e[1]))
  es.forEach(([a, b]) => g.setEdge(a, b))
  dagre.layout(g)
  const nodes: Node[] = ids.map((k) => { const p = g.node(k); return { id: k, type: "card", position: { x: p.x - W / 2, y: p.y - H / 2 }, data: { n: N[k], focus: k === focus }, draggable: true } })
  const edges: Edge[] = es.map(([a, b, l]) => {
    const fork = l.startsWith("派生"), ho = l === "交接", dep = l === "依赖", proj = l === "项目"
    const color = fork ? "var(--blue)" : proj ? "var(--border)" : "var(--tertiary)"
    return { id: a + ">" + b, source: a, target: b, type: "smoothstep", label: (!proj && l && edgeLabel(l)) || undefined, animated: false,
      style: { stroke: dep ? "var(--foreground)" : color, strokeDasharray: ho ? "5 4" : dep ? "1.5 3.5" : undefined, strokeLinecap: "round" as const, strokeWidth: dep ? 1.75 : undefined },
      markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color: dep ? "var(--foreground)" : color },
      labelStyle: { fontSize: 11, fill: fork ? "var(--blue)" : "var(--muted-foreground)", fontWeight: 500 },
      labelBgStyle: { fill: "var(--background)" }, labelBgPadding: [6, 3] as [number, number], labelBgBorderRadius: 6 }
  })
  return { nodes, edges }
}

export function RelationGraph({ focus, scope = "comp", height = 420, hideSimple = true, onOpen, minimal = false }: {
  focus?: string | null; scope?: "comp" | "near" | "all"; height?: number; hideSimple?: boolean; onOpen: (n: GNode) => void; minimal?: boolean
}) {
  const { N, E } = gBuild()
  const ids = useMemo(() => {
    if (scope === "near" && focus) return gNear(focus, E)
    let cs = gComps(N, E)
    if (scope === "comp" && focus) cs = cs.filter((c) => c.includes(focus))
    else { cs = cs.filter((c) => c.some((k) => N[k].k === "item")).sort((a, b) => b.length - a.length); if (hideSimple) cs = cs.filter((c) => c.length > 2) }
    return cs.flat()
  }, [JSON.stringify(E), scope, focus, hideSimple])
  const pj = gProjects(ids, N)
  const { nodes, edges } = layout([...Object.keys(pj.P), ...ids], [...pj.E, ...E], { ...N, ...pj.P }, focus)
  return (
    <div className="overflow-hidden rounded-xl border bg-canvas-graph" style={{ height }} data-graph>
      <ReactFlow key={ids.join()} nodes={nodes} edges={edges} nodeTypes={nodeTypes} fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} minZoom={0.3} maxZoom={1.6}
        nodesConnectable={false} elementsSelectable={false} proOptions={{ hideAttribution: true }} panOnScroll={false} zoomOnScroll={!minimal} preventScrolling={!minimal}
        onNodeClick={(_, node) => onOpen((node.data as any).n)}>
        {!minimal && <Controls showInteractive={false} position="bottom-right" />}
      </ReactFlow>
    </div>
  )
}

export function GraphLegend() {
  const t = useT()
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 type-footnote text-muted-foreground">
      {[[t("shell:graph.project"), "var(--border)", ""], [t("shell:graph.run"), "var(--tertiary)", ""], [t("shell:graph.handoff"), "var(--tertiary)", "4 3"], [t("shell:graph.fork"), "var(--blue)", ""], [t("shell:graph.dep"), "var(--foreground)", "1.5 3"]].map(([l, c, d]) => (
        <span key={l} className="inline-flex items-center gap-1.5"><svg width="20" height="6"><line x1="0" y1="3" x2="20" y2="3" stroke={c} strokeWidth="1.5" strokeDasharray={d} /></svg>{l}</span>
      ))}
      <span>{t("shell:graph.hint")}</span>
    </div>
  )
}
