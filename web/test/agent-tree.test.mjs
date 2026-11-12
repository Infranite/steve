import assert from "node:assert/strict"
import test from "node:test"
import { buildAgentTree, elapsed, findNode, rollup, summarize, unitStatus } from "../src/agent-tree.ts"

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
})

test("a missing root or a cycle of parents cannot hang the page", () => {
  assert.equal(buildAgentTree("nope", [item("ST-1")]), null)
  const items = [item("A", { parent: { item: "B" } }), item("B", { parent: { item: "A" } })]
  const tree = buildAgentTree("A", items)
  assert.deepEqual(tree.children.map((n) => n.key), ["B"])
  assert.deepEqual(tree.children[0].children, [], "A is already placed")
})

test("cost rolls up the Hub's whole subtree; time runs until the end", () => {
  const stat = (item_id, parent_id, est, extra = {}) => ({ item_id, parent_id, depth: 0, tokens: est ? 100 : 0, est, reported: 0, agent_ms: 5, first_turn: null, done_at: null, dropped_at: null, ...extra })
  const r = rollup([stat("A", null, 1), stat("B", "A", 0.5), stat("C", "B", 0.25), stat("D", "A", 0)])
  assert.equal(r.get("A").est, 1.75)
  assert.equal(r.get("B").est, 0.75)
  assert.equal(r.get("D").known, false)
  assert.equal(elapsed(stat("A", null, 0, { first_turn: 1000, done_at: 4000 }), "done", 9000), 3000)
  assert.equal(elapsed(stat("A", null, 0, { first_turn: 1000 }), "running", 9000), 8000)
  assert.equal(elapsed(stat("A", null, 0, { first_turn: 1000 }), "failed", 9000), 5, "an ended node without an end mark shows its agent time")
  assert.equal(elapsed(undefined, "running", 9000), null)
})

test("an item closed as failed is a failed node, not one still to start", () => {
  const tree = buildAgentTree("ST-1", [item("ST-1"), item("ST-2", { st: "fail", parent: { item: "ST-1" } })])
  assert.equal(tree.children[0].status, "failed")
})
