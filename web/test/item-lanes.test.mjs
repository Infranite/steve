import assert from "node:assert/strict"
import test from "node:test"
import { itemState } from "../src/item-state.ts"
import { besideState, laneOf, resumable, runStateOf } from "../src/item-lanes.ts"

const at = (i, running = false) => { const s = itemState(i, running); return [laneOf(s), runStateOf(s), resumable(s)] }

test("the board's column, the page's counts and the pill's row follow the list's groups", () => {
  assert.deepEqual(at({ st: "wait", waits: [{ q: "合并？" }] }), ["needs", "needs-you", false], "a card for you")
  assert.deepEqual(at({ st: "prop" }), ["needs", "needs-you", false], "a proposal")
  assert.deepEqual(at({ st: "pause" }), ["paused", "paused", true], "a paused item is not waiting on an answer; it resumes from the pill")
  assert.deepEqual(at({ st: "block" }), ["paused", "paused", false], "waiting for its prerequisites is neither running nor yours")
  assert.deepEqual(at({ st: "run" }, true), ["running", "running", false], "working")
  assert.deepEqual(at({ st: "run", q: [{}] }), ["running", "running", false], "your message queued")
  assert.deepEqual(at({ st: "run" }), ["running", "running", false], "open with nothing asked")
  assert.deepEqual(at({ st: "done" }), ["done", null, false])
  assert.deepEqual(at({ st: "fail" }), [null, null, false], "failed is closed: not on the board")
  assert.deepEqual(at({ st: "drop" }), [null, null, false])
})
test("a line beside its state says only the rest", () => {
  assert.equal(besideState("已暂停 · ST-41 失败", "已暂停"), "ST-41 失败")
  assert.equal(besideState("已暂停", "已暂停"), "")
  assert.equal(besideState("Paused: ST-41 failed", "Paused"), "ST-41 failed")
  assert.equal(besideState("跑测试", "进行中"), "跑测试")
})
