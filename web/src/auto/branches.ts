/* Branches of flow nodes and their edges (pure, no React: web/test/flow-branches.test.mjs runs them):
 * the labels a node's edges are expected to carry, renaming them with the node's branches, and the
 * same branch/edge checks as the hub (graph.rs `branch_warnings`). */
import type { GNode, Graph } from "@/api/flow"

const unq = (s: string) => s.trim().replace(/^"(.*)"$/, "$1")
/** labels a node's outgoing edges are expected to carry */
export function branchLabels(n: GNode): string[] {
  const c = n.config ?? {}
  switch (n.kind) {
    case "filter": return [...(c.rules ?? []).map((r: any) => r.label), c.else_label].filter(Boolean)
    case "bot": return c.branches ?? []
    case "approve": return c.options ?? []
    case "table": return [...new Set<string>((c.rows ?? []).map((r: string[]) => unq(r[(c.inputs ?? []).length] ?? "")).filter(Boolean))]
    case "wait": return c.timeout_label ? ["", c.timeout_label] : []
    default: return []
  }
}
/** a node's branches renamed in place (same count, a label changed at a position): its edges and its
 * `on_fail` branch follow, so renaming 是 to 需要 on the canvas keeps the path. An emptied or added or
 * removed label renames nothing. */
export const RENAMES_EDGES = new Set(["filter", "bot", "approve", "wait"])
export function renameBranchEdges(g: Graph, id: string, before: string[], after: string[]): Graph {
  if (before.length !== after.length) return g
  const map = new Map<string, string>()
  before.forEach((b, i) => { const a = after[i]; if (b && a && a !== b && !before.includes(a)) map.set(b, a) })
  if (!map.size) return g
  return {
    ...g,
    nodes: g.nodes.map((x) => { const l = x.id === id && x.on_fail?.startsWith("branch:") ? x.on_fail.slice(7) : null; return l !== null && map.has(l) ? { ...x, on_fail: "branch:" + map.get(l) } : x }),
    edges: g.edges.map((e) => (e.from === id && e.label && map.has(e.label) ? { ...e, label: map.get(e.label)! } : e)),
  }
}
/** the hub's branch/edge checks (graph.rs `branch_warnings`): an edge whose label is none of the node's
 * branches is never taken (strong); a branch without an edge ends the run there (weak) */
export type BranchWarning = { node: string; label: string; kind: "edge" | "branch"; strong: boolean }
export function branchWarnings(g: Graph): BranchWarning[] {
  const strong: BranchWarning[] = [], weak: BranchWarning[] = []
  for (const n of g.nodes) {
    const outs = g.edges.filter((e) => e.from === n.id), c = n.config ?? {}
    if (["filter", "bot", "approve"].includes(n.kind)) {
      const bs = [...new Set(branchLabels(n).map((b) => String(b ?? "").trim()).filter(Boolean))]
      if (bs.length) {
        const fail = n.on_fail?.startsWith("branch:") ? n.on_fail.slice(7) : null, seen = new Set<string>()
        for (const e of outs) if (e.label && !bs.includes(e.label) && e.label !== fail && !seen.has(e.label)) { seen.add(e.label); strong.push({ node: n.id, label: e.label, kind: "edge", strong: true }) }
        for (const b of bs) if (!outs.some((e) => e.label === b)) weak.push({ node: n.id, label: b, kind: "branch", strong: false })
      }
    }
    const tl = n.kind === "wait" && c.timeout ? String(c.timeout_label ?? "").trim() : ""
    if (tl && !outs.some((e) => e.label === tl)) weak.push({ node: n.id, label: tl, kind: "branch", strong: false })
  }
  return [...strong, ...weak]
}
/** the nodes the trigger can't reach (graph.rs `validate` refuses a graph with any: `flow.graph.unreachable`) */
export function unreachableNodes(g: Graph): string[] {
  const start = g.nodes.find((n) => n.kind === "trigger")?.id
  const seen = new Set<string>(start ? [start] : []), stack = start ? [start] : []
  while (stack.length) { const c = stack.pop()!; for (const e of g.edges) if (e.from === c && !seen.has(e.to)) { seen.add(e.to); stack.push(e.to) } }
  return g.nodes.filter((n) => !seen.has(n.id)).map((n) => n.id)
}
