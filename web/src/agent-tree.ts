/* ── the agent tree: who started whom (item page, collaboration) ──
 * One relation only. A work graph's coordinator started its units; a retry started a new attempt next to the
 * old one, which stays, dimmed; a fork started a new item from a point of another. Dependencies between units
 * are not edges here: a unit says what it is waiting for. Pure, so it is tested alone. */
import { orderedUnits, type Workflow, type WorkflowUnit } from "./workflow.ts"

/** the fields of a store `Item` the tree reads */
export type TreeItem = {
  id: string; t: string; b: string; st: string; phase?: string; arch?: boolean
  src: [string, string]; parent?: { item?: string; s?: string; at?: number } | null; waits?: unknown[]; ss?: string[]
}
/** one row of `GET /stats/tree/{id}`: an item's own figures, not its children's */
export type TreeStat = {
  item_id: string; parent_id: string | null; depth: number; tokens: number; est: number; unpriced_tokens?: number; reported: number
  agent_ms: number | null; first_turn: number | null; done_at: number | null; dropped_at: number | null
}
export type AgentStatus = "idle" | "proposed" | "pending" | "waiting" | "starting" | "running" | "unconfirmed" | "asking" | "paused" | "blocked" | "done" | "failed" | "stopped"
export type AgentKind = "root" | "unit" | "attempt" | "fork"
export type AgentNode = {
  /** unique in the tree: `unit:<id>` for a work graph's current unit (kept when its item appears or changes on
   * a retry), the item id for anything else */
  key: string
  kind: AgentKind
  item?: string
  unit?: WorkflowUnit
  /** an earlier attempt's unit, attempt number and reason, from the work graph's history */
  unitId?: string
  attempt?: number
  reason?: string | null
  session?: string | null
  title: string
  bot: string | null
  status: AgentStatus
  /** titles of the units this one waits for */
  waitingOn: string[]
  /** open cards on this node's item */
  decisions: number
  children: AgentNode[]
}

const ACTIVE: AgentStatus[] = ["starting", "running", "unconfirmed"]
export const isActive = (s: AgentStatus) => ACTIVE.includes(s)
export const isOver = (s: AgentStatus) => s === "done" || s === "failed" || s === "stopped"

const ITEM_STATUS: Record<string, AgentStatus> = { prop: "proposed", run: "running", wait: "asking", pause: "paused", block: "blocked", done: "done", drop: "stopped", fail: "failed" }
export function itemStatus(item: TreeItem): AgentStatus {
  return ITEM_STATUS[item.st] ?? "pending"
}

export function unitStatus(unit: WorkflowUnit, workflow: Workflow): AgentStatus {
  if (unit.status === "succeeded") return "done"
  if (unit.status === "failed") return "failed"
  if (unit.status === "cancelled" || unit.status === "canceled") return "stopped"
  if (workflow.paused) return "paused"
  if (unit.status === "dispatching") return "starting"
  if (unit.status === "running") return unit.activity?.state === "confirmed" ? "running" : "unconfirmed"
  if (unit.status === "blocked" || workflow.execution_ready === false) return "blocked"
  return waitingFor(unit, workflow).length ? "waiting" : "pending"
}

/** the units a pending unit still waits for (the Hub's own answer when it gave one) */
export function waitingFor(unit: WorkflowUnit, workflow: Workflow): string[] {
  return unit.waiting_on ?? unit.depends_on.filter((id) => workflow.units.find((u) => u.id === id)?.status !== "succeeded")
}

const SNAPSHOT_STATUS: Record<string, AgentStatus> = { succeeded: "done", failed: "failed", cancelled: "stopped", canceled: "stopped" }

/** `root` and everything started from it that the client knows: the current units of its work graph (an earlier
 * cycle's graph is history, not current work, but still who started whom), each unit's earlier attempts right
 * before it, and items forked from any node. Archived forks are left out; archived attempts are not. */
/** `rootStatus`: the item page's own state for the root (item-state.ts), so the tree and the page agree */
export function buildAgentTree(rootId: string, items: TreeItem[], workflow?: Workflow | null, depth = 8, rootStatus?: AgentStatus): AgentNode | null {
  const byId = new Map(items.map((i) => [i.id, i]))
  const root = byId.get(rootId)
  if (!root) return null
  const placed = new Set<string>([rootId])
  const forks = (id: string, level: number): AgentNode[] => level >= depth ? [] : items
    .filter((i) => i.parent?.item === id && i.src[1] !== "workflow" && !i.arch && !placed.has(i.id))
    .map((i) => { placed.add(i.id); return itemNode(i, "fork", level + 1) })
  const itemNode = (i: TreeItem, kind: AgentKind, level: number): AgentNode => ({
    key: i.id, kind, item: i.id, title: i.t, bot: i.b || null, status: itemStatus(i), session: i.ss?.[i.ss.length - 1] ?? null,
    waitingOn: [], decisions: i.waits?.length ?? 0, children: forks(i.id, level),
  })
  const children: AgentNode[] = []
  if (workflow) {
    for (const unit of orderedUnits(workflow.units)) {
      const old = (workflow.history_attempts ?? []).filter((a) => a.unit_id === unit.id && a.item_id !== unit.item_id && !placed.has(a.item_id))
        .sort((a, b) => a.snapshot.attempt - b.snapshot.attempt || a.created - b.created)
      for (const a of old) {
        placed.add(a.item_id)
        const item = byId.get(a.item_id)
        children.push({
          key: a.item_id, kind: "attempt", item: a.item_id, unitId: unit.id, attempt: a.snapshot.attempt, reason: a.snapshot.reason,
          session: a.snapshot.session_id ?? null, title: a.snapshot.title || unit.title, bot: item?.b || unit.bot_id,
          status: SNAPSHOT_STATUS[a.snapshot.status] ?? "stopped", waitingOn: [], decisions: 0, children: item ? forks(item.id, 1) : [],
        })
      }
      const item = unit.item_id ? byId.get(unit.item_id) : undefined
      if (unit.item_id) placed.add(unit.item_id)
      const status = unitStatus(unit, workflow)
      children.push({
        key: `unit:${unit.id}`, kind: "unit", item: unit.item_id ?? undefined, unit, unitId: unit.id, attempt: unit.attempt,
        reason: unit.reason, session: unit.session_id ?? null, title: unit.title, bot: unit.bot_id, status,
        waitingOn: status === "waiting" ? waitingFor(unit, workflow).map((id) => workflow.units.find((u) => u.id === id)?.title ?? id) : [],
        decisions: item?.waits?.length ?? 0, children: item ? forks(item.id, 1) : [],
      })
    }
  }
  children.push(...forks(rootId, 0))
  const node = itemNode(root, "root", 0)
  if (rootStatus) node.status = rootStatus
  // a coordinator whose current graph is paused is paused, whatever its own lifecycle says (as on the items list)
  if (workflow?.paused && workflow.current_cycle !== false && node.status === "running") node.status = "paused"
  return { ...node, children }
}

export function walk(node: AgentNode, f: (n: AgentNode, depth: number) => void, depth = 0) {
  f(node, depth)
  for (const c of node.children) walk(c, f, depth + 1)
}

export function findNode(node: AgentNode, key: string): AgentNode | null {
  if (node.key === key) return node
  for (const c of node.children) { const hit = findNode(c, key); if (hit) return hit }
  return null
}

export type TreeSummary = { agents: number; active: number; failed: AgentNode[]; asking: AgentNode[] }
/** the header line: every node counts as an agent, earlier attempts too; failures are current ones only */
export function summarize(root: AgentNode): TreeSummary {
  const s: TreeSummary = { agents: 0, active: 0, failed: [], asking: [] }
  walk(root, (n) => {
    s.agents++
    if (isActive(n.status)) s.active++
    if (n.status === "failed" && n.kind !== "attempt") s.failed.push(n)
    if (n !== root && n.decisions > 0) s.asking.push(n)
  })
  return s
}

export type Cost = { est: number; unpriced: number; reported: number; tokens: number; known: boolean }
/** each item's own cost plus everything started from it, by the Hub's tree (which also holds attempts the
 * page does not draw, so a parent's total is the full spend under it) */
export function rollup(stats: TreeStat[]): Map<string, Cost> {
  const kids = new Map<string, TreeStat[]>()
  for (const s of stats) if (s.parent_id) kids.set(s.parent_id, [...(kids.get(s.parent_id) ?? []), s])
  const out = new Map<string, Cost>()
  const total = (s: TreeStat, seen: Set<string>): Cost => {
    const hit = out.get(s.item_id)
    if (hit) return hit
    seen.add(s.item_id)
    const c: Cost = { est: s.est, unpriced: s.unpriced_tokens ?? 0, reported: s.reported, tokens: s.tokens, known: s.tokens > 0 || s.reported > 0 }
    for (const k of kids.get(s.item_id) ?? []) {
      if (seen.has(k.item_id)) continue
      const x = total(k, seen)
      c.est += x.est; c.unpriced += x.unpriced; c.reported += x.reported; c.tokens += x.tokens; c.known ||= x.known
    }
    out.set(s.item_id, c)
    return c
  }
  for (const s of stats) total(s, new Set())
  return out
}

/** how long the agent actually worked on it (its turns), not how long it has been open: a coordinator that has
 * waited on its units for a day did not work for a day */
export function workTime(s: TreeStat | undefined): number | null {
  return s?.agent_ms ?? null
}

/** a span of time an agent worked on a node: its first turn to its end (or now, while it runs) */
export type Segment = { key: string; item?: string; start: number; end: number; status: AgentStatus; attempt?: number }
/** one row of the collaboration: a unit with its earlier attempts folded into it, or a fork; `depth` indents
 * what a row's agent started itself */
export type TeamRow = { node: AgentNode; depth: number; tries: AgentNode[]; segments: Segment[]; ms: number }
const msOf = (t: number) => t < 1e12 ? t * 1000 : t
/** the tree as rows in order, with the spans of time from the Hub's figures (`GET /stats/tree`): earlier attempts
 * are not rows of their own but part of their unit's row, so a retried step reads as one step tried again */
export function teamRows(root: AgentNode, stats: TreeStat[], now: number): TeamRow[] {
  const by = new Map(stats.map((s) => [s.item_id, s]))
  const span = (n: AgentNode): Segment | null => {
    const s = n.item ? by.get(n.item) : undefined
    if (!s?.first_turn) return null
    const start = msOf(s.first_turn)
    const end = s.done_at ? msOf(s.done_at) : s.dropped_at ? msOf(s.dropped_at) : isActive(n.status) ? now : start + (s.agent_ms ?? 0)
    return { key: n.key, item: n.item, start, end: Math.max(start, end), status: n.status, attempt: n.attempt }
  }
  const worked = (n: AgentNode) => (n.item ? by.get(n.item)?.agent_ms : 0) ?? 0
  const out: TeamRow[] = []
  const add = (kids: AgentNode[], depth: number) => {
    let held: AgentNode[] = []
    for (const n of kids) {
      if (n.kind === "attempt") { held.push(n); continue }
      const tries = held.filter((a) => a.unitId && a.unitId === n.unitId)
      held = held.filter((a) => !tries.includes(a))
      const segments = [...tries, n].map(span).filter((x): x is Segment => !!x)
      out.push({ node: n, depth, tries, segments, ms: [...tries, n].reduce((s, x) => s + worked(x), 0) })
      for (const a of tries) add(a.children, depth + 1)
      add(n.children, depth + 1)
    }
    // attempts whose unit is gone from the graph still show, as rows of their own
    for (const a of held) out.push({ node: a, depth, tries: [], segments: [span(a)].filter((x): x is Segment => !!x), ms: worked(a) })
  }
  add(root.children, 0)
  return out
}

/** the first start and last end over all rows, or null before anything ran */
export function teamRange(rows: TeamRow[]): [number, number] | null {
  const xs = rows.flatMap((r) => r.segments)
  if (!xs.length) return null
  return [Math.min(...xs.map((s) => s.start)), Math.max(...xs.map((s) => s.end))]
}

/** one block on the collaboration's lanes: a unit's current attempt, one of its earlier attempts, a fork, or the
 * coordinator itself. `select` is the node a click opens (an earlier attempt opens its step, which lists its tries) */
export type LaneSpan = {
  key: string; select: string; node: AgentNode; bot: string | null; start: number; end: number; status: AgentStatus
  /** an earlier attempt of a step tried again */
  earlier: boolean
  /** the coordinator's own span; `cutStart`/`cutEnd` when it runs past the window on either side */
  coordinator: boolean; cutStart: boolean; cutEnd: boolean
  /** the blocks this one picked the work up from: a unit's direct dependencies (without the ones another of them
   * already waited for), or the block a fork was started from */
  after: string[]
}
export type TeamLanes = {
  /** the lanes in order: the coordinator's Bot first, then every Bot in the order the work reached it */
  bots: (string | null)[]; spans: LaneSpan[]
  /** every step in the order of the work, placed or not: the list under the lanes says what each was and how it
   * ended; `unplaced` are the ones with no span of time yet (not started, or no figures), not on the lanes */
  rows: TeamRow[]; unplaced: TeamRow[]
  /** the window the lanes show: the work's own first start to its last end, reaching now only while a step runs */
  range: [number, number] | null; live: boolean; worked: number; steps: number
  /** who started whom: a node's key to its parent's */
  parent: Map<string, string>
}

/** the work graph's dependencies of `unit` that are not already implied through another of them */
function directDeps(unit: WorkflowUnit, units: WorkflowUnit[]): string[] {
  const by = new Map(units.map((u) => [u.id, u]))
  const reach = (id: string, seen = new Set<string>()): Set<string> => {
    for (const d of by.get(id)?.depends_on ?? []) if (!seen.has(d)) { seen.add(d); reach(d, seen) }
    return seen
  }
  return unit.depends_on.filter((d) => !unit.depends_on.some((o) => o !== d && reach(o).has(d)))
}

/** the collaboration as lanes, one per Bot: every unit, attempt and fork that has a span of time is a block on its
 * Bot's lane; the coordinator's own span sits on its Bot's lane and is cut to the window. An earlier cycle's work
 * (`old`) is history and never reaches now. */
export function teamLanes(root: AgentNode, stats: TreeStat[], now: number, old = false): TeamLanes {
  const rows = teamRows(root, stats, now)
  const parent = new Map<string, string>()
  walk(root, (n) => { for (const c of n.children) parent.set(c.key, n.key) })
  const segOf = new Map<string, string>()
  const spans: LaneSpan[] = []
  const units = root.children.flatMap((n) => n.unit ? [n.unit] : [])
  for (const r of rows) {
    const nodes = [...r.tries, r.node]
    for (const s of r.segments) {
      const node = nodes.find((n) => n.key === s.key) ?? r.node
      segOf.set(node.key, s.key)
      spans.push({ key: s.key, select: r.node.key, node, bot: node.bot, start: s.start, end: s.end, status: s.status, earlier: node !== r.node, coordinator: false, cutStart: false, cutEnd: false, after: [] })
    }
  }
  for (const s of spans) {
    if (s.earlier) continue
    const n = s.node
    if (n.kind === "unit" && n.unit) s.after = directDeps(n.unit, units).map((id) => `unit:${id}`).filter((k) => segOf.has(k))
    else if (n.kind === "fork") { const p = parent.get(n.key); if (p && p !== root.key && segOf.has(p)) s.after = [p] }
  }
  const live = !old && spans.some((s) => isActive(s.status))
  const steps = rows.filter((r) => r.depth === 0).length
  const worked = rows.reduce((n, r) => n + r.ms, 0)
  const unplaced = rows.filter((r) => !r.segments.length)
  const rs = root.item ? stats.find((s) => s.item_id === root.item) : undefined
  let range: [number, number] | null = spans.length ? [Math.min(...spans.map((s) => s.start)), Math.max(...spans.map((s) => s.end))] : null
  let coordLive = false
  if (rs?.first_turn) {
    const start = msOf(rs.first_turn)
    const closed = rs.done_at ? msOf(rs.done_at) : rs.dropped_at ? msOf(rs.dropped_at) : null
    // the coordinator stays open while its work goes on: its span reaches now unless it closed. Over without a
    // recorded close, it ends with the last of its work, or – with none – its end is unknown and drawn cut (never
    // its working time added to a wall-clock start)
    const lastWork = spans.length ? Math.max(...spans.map((s) => s.end)) : null
    const known = closed ?? (isOver(root.status) ? lastWork : now)
    if (!range) { coordLive = !old && isActive(root.status); range = [start, coordLive ? now : known ?? start + 60_000] }
    if (live) range = [range[0], Math.max(range[1], now)]
    const end = Math.max(start, known ?? range[1])
    if (end > range[0] && start < range[1]) spans.unshift({
      key: root.key, select: root.key, node: root, bot: root.bot, start: Math.max(start, range[0]), end: Math.min(end, range[1]), status: root.status,
      earlier: false, coordinator: true, cutStart: start < range[0], cutEnd: end > range[1] || known === null, after: [],
    })
  } else if (live && range) range = [range[0], Math.max(range[1], now)]
  const bots: (string | null)[] = []
  for (const s of spans) if (!bots.includes(s.bot)) bots.push(s.bot)
  return { bots, spans, rows, unplaced, range, live: live || coordLive, worked, steps, parent }
}
