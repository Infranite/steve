/* The flow canvas: React Flow + dagre, the same look as the relation graph (web/src/graph.tsx), laid out left to right
 * (top to bottom on a phone). A trace (dry run or a past run) lights its path and dims the rest. */
import { useLayoutEffect, useMemo, useRef, useState } from "react"
import dagre from "@dagrejs/dagre"
import { ReactFlow, Controls, Handle, Position, MarkerType, type Edge, type Node, type NodeProps } from "@xyflow/react"
import { CheckIcon, XIcon } from "lucide-react"
import { cn } from "@/lib/utils"
import { Badge } from "@/components/ui/badge"
import type { GNode, Graph } from "@/api/flow"
import { kindOf, effectful, actionLabel } from "@/auto/nodes"
import { srcIcon } from "@/auto/common"
import { useT } from "@/i18n"

const NW = 224, NH = 76
type Ran = "ok" | "skip" | "fail" | "wait"
type D = { n: GNode; sel: boolean; fresh: boolean; ran?: Ran; narrow: boolean }

function FlowNode({ data }: NodeProps<Node<D>>) {
  const t = useT()
  const { n, sel, fresh, ran, narrow } = data, [label, I] = kindOf(n.kind)
  const Icon = n.kind === "trigger" ? srcIcon(n.config?.source) : I
  const sub = n.note || (n.kind === "action" ? actionLabel(n.config?.action) : "")
  return (
    <div className={cn("relative flex h-[76px] w-[224px] cursor-pointer items-center gap-3 rounded-xl bg-card px-3 shadow-[var(--lift)] transition-[box-shadow,opacity] duration-180 ease-(--ease-out) hover:shadow-[var(--lift-hover)]", sel && "shadow-[0_0_0_1.5px_var(--blue),0_0_0_5px_color-mix(in_srgb,var(--blue)_18%,transparent)]! ", fresh && "ring-3 ring-blue/30", ran === "skip" && "opacity-45")} data-rule-node={n.id} data-ran={ran}
      role="button" tabIndex={0} aria-label={t("auto:canvas.nodeAria", { kind: label, title: n.title })} aria-pressed={sel}>
      <Handle type="target" position={narrow ? Position.Top : Position.Left} className="!size-1.5 !min-h-0 !min-w-0 !border-0 !bg-border" />
      <span className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg", n.kind === "trigger" ? "bg-foreground text-background" : "bg-muted text-muted-foreground")}><Icon className="size-4" strokeWidth={1.75} /></span>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5 type-footnote text-muted-foreground">{label}
          {n.kind === "bot" && <Badge variant="secondary" className="h-4 rounded-md px-1 type-label leading-none text-blue bg-blue/10">AI</Badge>}
          {effectful(n) && <Badge variant="secondary" className="h-4 rounded-md bg-attention/12 px-1 type-label leading-none text-attention">{t("auto:canvas.needsApproval")}</Badge>}</span>
        <span className="block truncate type-callout font-medium">{n.title || n.id}</span>
        {sub && <span className="block truncate type-footnote text-muted-foreground">{sub}</span>}
      </span>
      {ran === "ok" && <CheckIcon className="absolute -right-1.5 -top-1.5 size-4 rounded-full bg-success p-0.5 text-white" strokeWidth={3} aria-label={t("auto:canvas.ran")} />}
      {ran === "fail" && <XIcon className="absolute -right-1.5 -top-1.5 size-4 rounded-full bg-destructive p-0.5 text-white" strokeWidth={3} aria-label={t("auto:runStatus.failed")} />}
      {ran === "wait" && <span className="absolute -right-1 -top-1 size-3 rounded-full bg-attention ring-2 ring-card" aria-label={t("auto:runStatus.waiting")} />}
      <Handle type="source" position={narrow ? Position.Bottom : Position.Right} className="!size-1.5 !min-h-0 !min-w-0 !border-0 !bg-border" />
    </div>
  )
}
const flowTypes = { rn: FlowNode }

/** trace: node id → how it ran; path edges = consecutive trace entries */
export function FlowCanvas({ g, sel, onSel, fresh, trace, height = 460 }: { g: Graph; sel: string | null; onSel: (id: string) => void; fresh: string | null; trace: { path: string[]; ran: Record<string, Ran> } | null; height?: number }) {
  const box = useRef<HTMLDivElement>(null), [w, setW] = useState(0)
  useLayoutEffect(() => {
    const el = box.current; if (!el) return
    const ro = new ResizeObserver(() => setW(el.clientWidth)); ro.observe(el); setW(el.clientWidth); return () => ro.disconnect()
  }, [])
  const narrow = w > 0 && w < 560
  const { nodes, edges } = useMemo(() => {
    const d = new dagre.graphlib.Graph(); d.setGraph({ rankdir: narrow ? "TB" : "LR", nodesep: 26, ranksep: narrow ? 56 : 72, marginx: 16, marginy: 16 }); d.setDefaultEdgeLabel(() => ({}))
    g.nodes.forEach((n) => d.setNode(n.id, { width: NW, height: NH })); g.edges.forEach((e) => { if (d.hasNode(e.from) && d.hasNode(e.to)) d.setEdge(e.from, e.to) }); dagre.layout(d)
    const nodes: Node[] = g.nodes.map((n) => { const p = d.node(n.id); return { id: n.id, type: "rn", position: { x: p.x - NW / 2, y: p.y - NH / 2 }, data: { n, sel: n.id === sel, fresh: n.id === fresh, narrow, ran: trace ? trace.ran[n.id] ?? "skip" : undefined } } })
    const hot = (a: string, b: string) => !!trace && trace.path.some((x, i) => x === a && trace.path[i + 1] === b)
    const edges: Edge[] = g.edges.map((e) => { const h = hot(e.from, e.to); return { id: e.from + ">" + e.to, source: e.from, target: e.to, type: "smoothstep", label: e.label || undefined,
      style: { stroke: h ? "var(--blue)" : "var(--tertiary)", strokeWidth: h ? 2 : undefined, opacity: trace && !h ? 0.5 : 1 }, markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color: h ? "var(--blue)" : "var(--tertiary)" },
      labelStyle: { fontSize: 11, fill: h ? "var(--blue)" : "var(--muted-foreground)", fontWeight: 500 }, labelBgStyle: { fill: "var(--canvas-graph)" }, labelBgPadding: [6, 3] as [number, number], labelBgBorderRadius: 6 } })
    return { nodes, edges }
  }, [JSON.stringify(g), sel, fresh, JSON.stringify(trace), narrow])
  /* initial view: fit when readable (≥ 55%), otherwise start at the trigger at 55% and let the user pan */
  const vp = useMemo(() => {
    if (!w || !nodes.length) return { x: 0, y: 0, zoom: 1 }
    const H = box.current?.clientHeight ?? height, xs = nodes.map((x) => x.position.x), ys = nodes.map((x) => x.position.y)
    const minX = Math.min(...xs), minY = Math.min(...ys), bw = Math.max(...xs) + NW - minX, bh = Math.max(...ys) + NH - minY
    const fit = Math.min(1, (w - 48) / bw, (H - 48) / bh), z = Math.max(fit, 0.55)
    const x = fit >= 0.55 ? (w - bw * z) / 2 - minX * z : 24 - minX * z, y = bh * z <= H - 48 ? (H - bh * z) / 2 - minY * z : 24 - minY * z
    return { x, y, zoom: z }
  }, [w, nodes.length, narrow])
  return (
    <div ref={box} className="overflow-hidden rounded-xl bg-canvas-graph shadow-[inset_0_0_0_1px_var(--border)]" style={{ height }} data-rule-canvas>
      {w > 0 && <ReactFlow key={g.nodes.length + ":" + g.edges.length + ":" + (narrow ? "n" : "w")} nodes={nodes} edges={edges} nodeTypes={flowTypes} minZoom={0.3} maxZoom={1.5} defaultViewport={vp}
        nodesConnectable={false} nodesDraggable={false} elementsSelectable={false} proOptions={{ hideAttribution: true }} onNodeClick={(_, n) => onSel(n.id)}
        onKeyDown={(e) => { const id = (e.target as HTMLElement).closest("[data-rule-node]")?.getAttribute("data-rule-node"); if (id && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); onSel(id) } }}>
        <Controls showInteractive={false} position="bottom-right" />
      </ReactFlow>}
    </div>
  )
}
