import assert from "node:assert/strict"
import test from "node:test"
import { itemState } from "../src/item-state.ts"
import { besideState, laneOf, planProgress, resumable, runStateOf } from "../src/item-lanes.ts"

const wf = (x = {}) => ({ item_id: "ST-1", revision: 1, paused: false, max_parallel: 1, max_attempts: 3, units: [], blockers: [], history: [], ...x })
const at = (i, w = null, running = false) => { const s = itemState(i, w, running); return [laneOf(s), runStateOf(s), resumable(s)] }

test("the board's column, the page's counts and the pill's row follow the list's groups", () => {
  assert.deepEqual(at({ st: "wait", waits: [{ q: "合并？" }] }), ["needs", "needs-you", false], "a card for you")
  assert.deepEqual(at({ st: "prop" }), ["needs", "needs-you", false], "a proposal")
  assert.deepEqual(at({ st: "run" }, wf({ execution_ready: false, blockers: ["Node 离线"] })), ["needs", "needs-you", false], "a graph that cannot execute until you fix it")
  assert.deepEqual(at({ st: "pause" }), ["paused", "paused", true], "a paused item is not waiting on an answer; it resumes from the pill")
  assert.deepEqual(at({ st: "run" }, wf({ paused: true, blockers: ["ST-43 失败"] })), ["paused", "paused", false], "a paused graph resumes on its page")
  assert.deepEqual(at({ st: "block" }), ["paused", "paused", false], "waiting for its prerequisites is neither running nor yours")
  assert.deepEqual(at({ st: "run" }, null, true), ["running", "running", false], "working")
  assert.deepEqual(at({ st: "run", q: [{}] }), ["running", "running", false], "your message queued")
  assert.deepEqual(at({ st: "run" }), ["running", "running", false], "open with nothing asked")
  assert.deepEqual(at({ st: "done" }), ["done", null, false])
  assert.deepEqual(at({ st: "fail" }), [null, null, false], "failed is closed: not on the board")
  assert.deepEqual(at({ st: "drop" }), [null, null, false])
})

test("a line beside its state says only the rest", () => {
  assert.equal(besideState("协作已暂停 · ST-41 失败", "协作已暂停"), "ST-41 失败")
  assert.equal(besideState("协作已暂停", "协作已暂停"), "")
  assert.equal(besideState("Collaboration paused: ST-41 failed", "Collaboration paused"), "ST-41 failed")
  assert.equal(besideState("跑测试", "进行中"), "跑测试")
})

test("plan progress counts finished steps; no plan, no progress", () => {
  assert.deepEqual(planProgress([["a", 1], ["b", 0], ["c", 1]]), { done: 2, total: 3 })
  assert.equal(planProgress(undefined), null)
  assert.equal(planProgress([]), null)
})
