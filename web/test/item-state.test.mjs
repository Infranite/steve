import assert from "node:assert/strict"
import test from "node:test"
import { itemState } from "../src/item-state.ts"

const wf = (x = {}) => ({ item_id: "ST-1", revision: 1, paused: false, max_parallel: 1, max_attempts: 3, units: [], blockers: [], history: [], ...x })

test("one state answers whose move it is, and the page cannot disagree with itself", () => {
  // ST-27 as it was: lifecycle "run", graph from an earlier cycle (paused and unavailable), a message of yours queued
  const st27 = itemState({ st: "run", now: "协作已暂停", q: [{}] }, wf({ current_cycle: false, paused: true, execution_ready: false, blockers: ["x"] }), false)
  assert.deepEqual([st27.kind, st27.move], ["queued", "agents"], "an earlier cycle's graph is history; the item goes on in the current cycle")
  assert.equal(itemState({ st: "run" }, wf({ paused: true, blockers: ["独立评审未通过"] }), false).detail, "独立评审未通过")
  assert.equal(itemState({ st: "run" }, wf({ execution_ready: false, paused: true }), false).kind, "unavailable")
})

test("a card for you comes before anything running; running comes before a quiet graph", () => {
  assert.equal(itemState({ st: "run", waits: [{ q: "批准吗？" }] }, wf({ paused: true }), true).kind, "asking")
  assert.equal(itemState({ st: "run", now: "写代码" }, wf({ paused: true }), true).kind, "working")
  assert.equal(itemState({ st: "run" }, wf({ acceptance_mode: "single_session", paused: true }), false).kind, "idle", "single-session acceptance has no graph to resume")
})

test("lifecycle states without a graph", () => {
  assert.equal(itemState({ st: "done" }, null, false).move, "none")
  assert.equal(itemState({ st: "fail", now: "构建失败" }, null, false).detail, "构建失败")
  assert.equal(itemState({ st: "pause" }, null, false).kind, "paused")
  assert.equal(itemState({ st: "block" }, null, false).kind, "blocked")
  assert.equal(itemState({ st: "run", q: [{}] }, null, false).kind, "queued")
  assert.equal(itemState({ st: "run", now: "等它继续" }, null, false).kind, "idle")
})
