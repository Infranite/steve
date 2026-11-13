import assert from "node:assert/strict"
import test from "node:test"
import { buildAgentTree, workTime, findNode, rollup, summarize, teamLanes, teamRange, teamRows, unitStatus } from "../src/agent-tree.ts"

const item = (id, extra = {}) => ({ id, t: `title ${id}`, b: "dev", st: "run", src: ["Steve", "chat"], waits: [], ...extra })
const unit = (id, status, extra = {}) => ({ id, title: `unit ${id}`, goal: "", bot_id: "dev", kind: "work", depends_on: [], resources: [], required: true, status, attempt: 1, ...extra })
const workflow = (units, extra = {}) => ({ item_id: "ST-1", revision: 1, paused: false, max_parallel: 2, max_attempts: 3, units, blockers: [], history: [], ...extra })

test("units hang under the coordinator in dependency order, earlier attempts right before the current one", () => {
  const wf = workflow([
    unit("review", "pending", { depends_on: ["api"], kind: "review" }),
    unit("api", "running", { item_id: "ST-3", attempt: 2, activity: { state: "confirmed" } }),
  ], { history_attempts: [{ item_id: "ST-2", unit_id: "api", created: 1, snapshot: { title: "old api", status: "failed", attempt: 1, reason: "tests failed" } }] })
  const items = [item("ST-1"), item("ST-2", { src: ["Steve", "workflow"], parent: { item: "ST-1" }, arch: true }), item("ST-3", { src: ["Steve", "workflow"], parent: { item: "ST-1" }, waits: [{}] }),
    item("ST-4", { parent: { item: "ST-3", s: "s", at: 4 } }), item("ST-5", { parent: { item: "ST-1" }, arch: true })]
  const tree = buildAgentTree("ST-1", items, wf)
  assert.deepEqual(tree.children.map((n) => [n.key, n.kind, n.status]), [
    ["ST-2", "attempt", "failed"], ["unit:api", "unit", "running"], ["unit:review", "unit", "waiting"],
  ], "an archived fork is not drawn; an archived attempt is")
  assert.deepEqual(tree.children[1].children.map((n) => [n.key, n.kind]), [["ST-4", "fork"]], "a fork hangs under the item it was started from")
  assert.deepEqual(tree.children[2].waitingOn, ["unit api"])
  assert.equal(tree.children[0].reason, "tests failed")
  const s = summarize(tree)
  assert.equal(s.agents, 5)
  assert.equal(s.active, 3, "the coordinator, the running unit and its running fork")
  assert.deepEqual(s.failed, [], "an earlier failed attempt is history, not a current failure")
  assert.deepEqual(s.asking.map((n) => n.item), ["ST-3"])
  assert.equal(findNode(tree, "ST-4").title, "title ST-4")
})

test("unit status: paused and unavailable graphs say so; activity decides running", () => {
  const wf = workflow([unit("a", "running")])
  assert.equal(unitStatus(wf.units[0], wf), "unconfirmed")
  assert.equal(unitStatus({ ...wf.units[0], activity: { state: "confirmed" } }, wf), "running")
  assert.equal(unitStatus(wf.units[0], { ...wf, paused: true }), "paused")
  assert.equal(unitStatus(unit("b", "pending"), { ...wf, execution_ready: false }), "blocked")
  assert.equal(unitStatus(unit("c", "succeeded"), { ...wf, paused: true }), "done", "a result stays a result while paused")
  assert.equal(unitStatus(unit("d", "cancelled"), wf), "stopped")
  const root = (w) => buildAgentTree("ST-1", [item("ST-1")], w).status
  assert.equal(root({ ...wf, paused: true }), "paused", "a running coordinator of a paused graph is paused")
  assert.equal(root({ ...wf, paused: true, current_cycle: false }), "running", "an earlier cycle's pause is history")
})

test("a missing root or a cycle of parents cannot hang the page", () => {
  assert.equal(buildAgentTree("nope", [item("ST-1")]), null)
  const items = [item("A", { parent: { item: "B" } }), item("B", { parent: { item: "A" } })]
  const tree = buildAgentTree("A", items)
  assert.deepEqual(tree.children.map((n) => n.key), ["B"])
  assert.deepEqual(tree.children[0].children, [], "A is already placed")
})

test("cost rolls up the Hub's whole subtree; time is the agent's working time", () => {
  const stat = (item_id, parent_id, est, extra = {}) => ({ item_id, parent_id, depth: 0, tokens: est ? 100 : 0, est, reported: 0, agent_ms: 5, first_turn: null, done_at: null, dropped_at: null, ...extra })
  const r = rollup([stat("A", null, 1), stat("B", "A", 0.5), stat("C", "B", 0.25), stat("D", "A", 0)])
  assert.equal(r.get("A").est, 1.75)
  assert.equal(r.get("B").est, 0.75)
  assert.equal(r.get("D").known, false)
  const u = rollup([stat("A", null, 1, { unpriced_tokens: 0 }), stat("B", "A", 0, { unpriced_tokens: 7_400_000 })])
  assert.equal(u.get("A").unpriced, 7_400_000, "unpriced tokens under a node are part of its total")
  assert.equal(workTime(stat("A", null, 0, { first_turn: 1000, agent_ms: 3000 })), 3000, "working time, not time since the first turn")
  assert.equal(workTime(undefined), null)
})

test("an item closed as failed is a failed node, not one still to start", () => {
  const tree = buildAgentTree("ST-1", [item("ST-1"), item("ST-2", { st: "fail", parent: { item: "ST-1" } })])
  assert.equal(tree.children[0].status, "failed")
})

test("team rows: a retried step is one row with every attempt's span; forks follow their row", () => {
  const wf = workflow([
    unit("api", "succeeded", { item_id: "ST-3", attempt: 2 }),
    unit("review", "pending", { depends_on: ["api"], kind: "review" }),
  ], { history_attempts: [{ item_id: "ST-2", unit_id: "api", created: 1, snapshot: { title: "old api", status: "cancelled", attempt: 1 } }] })
  const items = [item("ST-1"), item("ST-2", { src: ["Steve", "workflow"], parent: { item: "ST-1" } }), item("ST-3", { src: ["Steve", "workflow"], parent: { item: "ST-1" }, st: "done" }),
    item("ST-4", { parent: { item: "ST-3", s: "s", at: 4 } })]
  const tree = buildAgentTree("ST-1", items, wf)
  const stat = (id, first, done, ms, dropped = null) => ({ item_id: id, parent_id: "ST-1", depth: 1, tokens: 1, est: 0, reported: 0, agent_ms: ms, first_turn: first, done_at: done, dropped_at: dropped })
  const T = 1_700_000_000_000
  const rows = teamRows(tree, [stat("ST-2", T / 1000, null, 60_000), stat("ST-3", T + 200_000, T + 500_000, 120_000), stat("ST-4", T + 600_000, null, 1000)], T + 900_000)
  assert.deepEqual(rows.map((r) => [r.node.key, r.depth, r.tries.length]), [["unit:api", 0, 1], ["ST-4", 1, 0], ["unit:review", 0, 0]])
  assert.deepEqual(rows[0].segments.map((s) => [s.status, s.start - T, s.end - T]), [["stopped", 0, 60_000], ["done", 200_000, 500_000]], "seconds become milliseconds; a span without an end lasts its working time")
  assert.equal(rows[0].ms, 180_000, "the row's time holds every attempt")
  assert.equal(rows[1].segments[0].end, T + 900_000, "a running fork reaches now")
  assert.deepEqual(rows[2].segments, [], "a step not started has no bar")
  assert.deepEqual(teamRange(rows), [T, T + 900_000])
})

test("team lanes: a lane per Bot, a window that fits the work, hand-offs from direct dependencies only", () => {
  const T = 1_700_000_000_000
  const wf = workflow([
    unit("a", "succeeded", { item_id: "ST-2", bot_id: "dev" }),
    unit("b", "succeeded", { item_id: "ST-3", bot_id: "doc", depends_on: ["a"] }),
    unit("c", "succeeded", { item_id: "ST-4", bot_id: "rev", depends_on: ["a", "b"] }),
    unit("d", "pending", { bot_id: "dev", depends_on: ["c"] }),
  ], { current_cycle: false })
  const items = [item("ST-1", { b: "lead" }), ...["ST-2", "ST-3", "ST-4"].map((id) => item(id, { src: ["Steve", "workflow"], parent: { item: "ST-1" }, st: "done" }))]
  const tree = buildAgentTree("ST-1", items, wf, 8, "running")
  const stat = (id, first, done, ms = 1000) => ({ item_id: id, parent_id: id === "ST-1" ? null : "ST-1", depth: 1, tokens: 1, est: 0, reported: 0, agent_ms: ms, first_turn: first, done_at: done, dropped_at: null })
  const H = 3_600_000
  const stats = [stat("ST-1", T - H, null), stat("ST-2", T, T + H), stat("ST-3", T + H, T + 2 * H), stat("ST-4", T + 2 * H, T + 3 * H)]
  const lanes = teamLanes(tree, stats, T + 48 * H, true)
  assert.deepEqual(lanes.bots, ["lead", "dev", "doc", "rev"], "the coordinator's Bot first, then in the order the work reached them")
  assert.deepEqual(lanes.range, [T, T + 3 * H], "an earlier round's window is its own hours, not up to now")
  assert.equal(lanes.live, false)
  const coord = lanes.spans.find((s) => s.coordinator)
  assert.deepEqual([coord.start, coord.end, coord.cutStart, coord.cutEnd], [T, T + 3 * H, true, true], "the coordinator is cut to the window")
  assert.deepEqual(lanes.spans.find((s) => s.key === "unit:c").after, ["unit:b"], "a hand-off already implied through another dependency is not drawn")
  assert.deepEqual(lanes.unplaced.map((r) => r.node.key), ["unit:d"], "a step not started is listed, not placed")
  const now = T + 5 * H
  const running = teamLanes(buildAgentTree("ST-1", items, { ...wf, current_cycle: true, units: wf.units.map((u) => u.id === "c" ? { ...u, status: "running", activity: { state: "confirmed" } } : u) }), [...stats.slice(0, 3), stat("ST-4", T + 2 * H, null)], now)
  assert.equal(running.live, true)
  assert.equal(running.range[1], now, "while a step runs the window reaches now")
  const ended = teamLanes(buildAgentTree("ST-1", items, wf, 8, "done"), stats, T + 48 * H, true)
  const end = ended.spans.find((s) => s.coordinator)
  assert.deepEqual([end.end, end.cutEnd], [T + 3 * H, false], "a coordinator over without a recorded close ends with the last of its work")
  const alone = teamLanes(buildAgentTree("ST-1", [items[0]], null, 8, "done"), [stat("ST-1", T, null, 5 * H)], T + 48 * H)
  const lone = alone.spans.find((s) => s.coordinator)
  assert.ok(lone.cutEnd && lone.end - lone.start < 5 * H, "with no work of its own and no close, its end is unknown: cut, never its working time from its start")
})
