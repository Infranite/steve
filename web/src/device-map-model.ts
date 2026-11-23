/* The connection map's reading of the devices (devices-map.tsx): which remote Nodes and clients it draws and in what
 * order, the edge each Node's status gives it, and the edges' geometry from the cards' boxes. Pure, so the order and
 * the shapes do not depend on the page; web/test/device-map-model.test.mjs pins it. Imports use .ts so node --test
 * loads it. Nothing here is random or force-directed: the same data always gives the same map. */
import type { AccessClient, AccessView } from "./access-api.ts"
import { nodeStatus, type NodeStatus, type NodeView } from "./device-model.ts"

/** from this many remote Nodes on, a card is one line (state dot, name, what matters, version) */
export const MAP_COMPACT_FROM = 7
/** at most this many remote Nodes are drawn; the rest fold into "N more" */
export const MAP_MAX_NODES = 12
/** at most this many clients are drawn (you, the online ones, the latest); the rest fold into "N more" */
export const MAP_MAX_CLIENTS = 4
/** an execution edge's width: this at no session, … */
const EDGE_MIN = 1.6
/** … this more per running session, … */
const EDGE_PER_SESSION = 0.45
/** … and never more than this */
const EDGE_MAX = 4

/** how a Hub → Node edge is drawn: amber dots (fingerprint to confirm), grey dots (offline), blue moving dashes
 *  (upgrading), red (upgrade failed), neutral (not taking work), amber (behind the Hub or queued), green (online) */
export type EdgeKind = "pending" | "off" | "upgrading" | "bad" | "neutral" | "warn" | "ok"
export function edgeOf(s: NodeStatus): EdgeKind {
  switch (s.kind) {
    case "pending": return "pending"
    case "offline": case "revoked": return "off"
    case "upgrading": return "upgrading"
    case "rolledBack": return "bad"
    case "paused": return "neutral"
    case "outdated": case "queued": return "warn"
    default: return "ok"
  }
}
/** the stroke width of an execution edge: thicker with each running session; an edge to a Node that is away is thin */
export const edgeWidth = (v: Pick<NodeView, "linked" | "running">) => (v.linked ? Math.min(EDGE_MIN + EDGE_PER_SESSION * (v.running ?? 0), EDGE_MAX) : 1.5)
/** sessions are running over the edge: its dashes move (still when motion is reduced) */
export const edgeFlows = (v: Pick<NodeView, "linked" | "running">, kind: EdgeKind) => kind === "ok" && v.linked && (v.running ?? 0) > 0

/** where a Node goes in the map's list: what needs you (went wrong, wants a step, under way), then online and busy,
 *  online and idle, not taking work, and away last */
export function mapRank(v: NodeView, hubVersion = ""): number {
  const s = nodeStatus(v, hubVersion)
  if (s.tone === "bad") return 0
  if (s.tone === "warn") return 1
  if (s.tone === "info") return 2
  if (s.kind === "online") return (v.running ?? 0) > 0 ? 3 : 4
  if (s.kind === "paused") return 5
  return 6
}

/** the remote Nodes the map draws, in its order: by rank, then the busier first, then by name */
export function mapNodes(views: readonly NodeView[], hubVersion = ""): NodeView[] {
  return views.filter((v) => !v.local && v.state !== "revoked")
    .map((v) => ({ v, r: mapRank(v, hubVersion) }))
    .sort((a, b) => a.r - b.r || (b.v.running ?? 0) - (a.v.running ?? 0) || a.v.name.localeCompare(b.v.name))
    .map((x) => x.v)
}

/** Keeps the order a list had when the page opened: ids seen before keep their place, new ones join at the end in the
 *  order given, gone ones drop out. So a card never jumps while it is looked at; reopening the page sorts again. */
export function keepOrder(before: readonly string[], now: readonly string[]): string[] {
  const here = new Set(now)
  const kept = before.filter((id) => here.has(id)), known = new Set(kept)
  return [...kept, ...now.filter((id) => !known.has(id))]
}

/** a client holds the live connection to the Hub now: as the Hub says (newer Hubs), else only the one in use */
export const clientOnline = (c: AccessClient) => c.online

/** one client as a person sees it: the same name, kind, platform, route, person and token (a browser that signed in
 *  again and again is one client) */
export const clientKey = (c: AccessClient) => [c.user_id, c.name, c.kind, c.platform, c.last_route, c.token_id].join("\u0000")

/** the id the map gives this browser when its session is not a registered client (a cookie sign-in) */
export const SELF_CLIENT = "self"

/** The clients the map draws: signed in (not revoked), one per client, this one first, then the online ones, then the
 *  latest; at most MAP_MAX_CLIENTS. `total`: how many there are in all. A session that is no registered client (a
 *  cookie sign-in) is still this one, online, named `selfName`. */
export function mapClients(view: AccessView | null | undefined, selfName = ""): { shown: AccessClient[]; total: number; online: number } {
  if (!view) return { shown: [], total: 0, online: 0 }
  const v = view.clients.some((c) => c.id === view.current && !c.revoked) ? view
    : { ...view, current: SELF_CLIENT, clients: [{ id: SELF_CLIENT, kind: "web", name: selfName, platform: "", version: "", origin: null, created: 0, last_seen: null, last_route: view.route, last_ip: null, revoked: null, online: true, streams: 0, connected_since: null }, ...view.clients] }
  const seen = (c: AccessClient) => c.last_seen ?? c.created
  const byKey = new Map<string, AccessClient>()
  for (const c of v.clients) {
    if (c.revoked) continue
    const k = c.id === v.current ? "\u0001current" : clientKey(c), had = byKey.get(k)
    if (!had || (clientOnline(c) && !clientOnline(had)) || seen(c) > seen(had)) byKey.set(k, c)
  }
  const all = [...byKey.values()].sort((a, b) => Number(b.id === v.current) - Number(a.id === v.current)
    || Number(clientOnline(b)) - Number(clientOnline(a)) || seen(b) - seen(a))
  return { shown: all.slice(0, MAP_MAX_CLIENTS), total: all.length, online: all.filter((c) => clientOnline(c)).length }
}

/** a card's box inside the map, in px */
export type Box = { l: number; t: number; r: number; b: number }
const r1 = (x: number) => Math.round(x * 10) / 10
const mid = (b: Box) => (b.t + b.b) / 2

/** a curve that leaves (x1, y1) and enters (x2, y2) horizontally: edges from one point fan out and every one reaches its
 *  card level, so the labels at the card ends never overlap */
export function hCurve(x1: number, y1: number, x2: number, y2: number): string {
  const dx = (x2 - x1) / 2
  return `M${r1(x1)},${r1(y1)} C${r1(x1 + dx)},${r1(y1)} ${r1(x2 - dx)},${r1(y2)} ${r1(x2)},${r1(y2)}`
}
/** the same, leaving and entering vertically (a phone's clients above the Hub) */
export function vCurve(x1: number, y1: number, x2: number, y2: number): string {
  const dy = (y2 - y1) / 2
  return `M${r1(x1)},${r1(y1)} C${r1(x1)},${r1(y1 + dy)} ${r1(x2)},${r1(y2 - dy)} ${r1(x2)},${r1(y2)}`
}

/** one edge of the map: its id (a Node's or a client's, `local` for the Hub's own Node, `add` for the placeholder) */
export type MapEdge = { id: string; d: string; kind: "exec" | "access"; width?: number }
/** a route label: centred on (x, y) */
export type MapLabel = { id: string; x: number; y: number }

/** Desktop: clients on the left run to the Hub's left side; the Hub's right side fans out to every remote Node (and the
 *  placeholder), each entering its card's left side; the Hub's own Node hangs under it. Labels sit at the Node ends. */
export function wideEdges(hub: Box, local: Box | null, nodes: { id: string; box: Box }[], clients: { id: string; box: Box }[]) {
  const edges: Omit<MapEdge, "kind" | "width">[] = [], access: Omit<MapEdge, "kind">[] = [], labels: MapLabel[] = []
  const cx = (hub.l + hub.r) / 2
  if (local) { edges.push({ id: "local", d: `M${r1(cx)},${r1(hub.b)} L${r1(cx)},${r1(local.t)}` }); labels.push({ id: "local", x: cx, y: (hub.b + local.t) / 2 }) }
  for (const n of nodes) {
    edges.push({ id: n.id, d: hCurve(hub.r, mid(hub), n.box.l, mid(n.box)) })
    labels.push({ id: n.id, x: n.box.l - 34, y: mid(n.box) })
  }
  for (const c of clients) access.push({ id: c.id, d: hCurve(c.box.r, mid(c.box), hub.l, mid(hub)) })
  return { edges, access, labels }
}

/** Phone: the clients above run down into the top of the Hub's frame, spread over its width; the Hub's own Node under
 *  the Hub; remote Nodes hang off one trunk down the frame's left side, like a file tree, each branch entering its
 *  card. No labels on the branches: a phone's card says its route itself. */
export function narrowEdges(hub: Box, frame: Box, local: Box | null, nodes: { id: string; box: Box }[], clients: { id: string; box: Box }[]) {
  const edges: Omit<MapEdge, "kind" | "width">[] = [], access: Omit<MapEdge, "kind">[] = [], labels: MapLabel[] = []
  const cx = (hub.l + hub.r) / 2
  if (local) { edges.push({ id: "local", d: `M${r1(cx)},${r1(hub.b)} L${r1(cx)},${r1(local.t)}` }); labels.push({ id: "local", x: cx, y: (hub.b + local.t) / 2 }) }
  clients.forEach((c, i) => {
    const x2 = hub.l + ((i + 1) * (hub.r - hub.l)) / (clients.length + 1)
    access.push({ id: c.id, d: vCurve((c.box.l + c.box.r) / 2, c.box.b, x2, frame.t) })
  })
  let trunk: string | null = null
  if (nodes.length) {
    const tx = frame.l + 10, last = mid(nodes[nodes.length - 1].box)
    trunk = `M${r1(tx)},${r1(frame.b)} L${r1(tx)},${r1(last - 8)}`
    for (const n of nodes) {
      const y = mid(n.box)
      edges.push({ id: n.id, d: `M${r1(tx)},${r1(y - 8)} Q${r1(tx)},${r1(y)} ${r1(tx + 8)},${r1(y)} L${r1(n.box.l)},${r1(y)}` })
    }
  }
  return { edges, access, labels, trunk }
}
