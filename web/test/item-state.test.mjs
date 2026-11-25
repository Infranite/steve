import assert from "node:assert/strict"
import test from "node:test"
import { itemDisplayStatusOf, itemState } from "../src/item-state.ts"

test("a card for you comes before anything running; running comes before paused or idle", () => {
  assert.equal(itemState({ st: "run", waits: [{ q: "批准吗？" }] }, true).kind, "asking")
  assert.equal(itemState({ st: "run", progress: "写代码" }, true).kind, "working")
  assert.equal(itemState({ st: "run", q: [{}] }, false).kind, "queued")
  assert.equal(itemDisplayStatusOf(itemState({ st: "run", waits: [{ q: "批准吗？" }] }, true)), "wait")
})

test("lifecycle states", () => {
  assert.equal(itemState({ st: "done" }, false).move, "none")
  assert.equal(itemState({ st: "fail", progress: "构建失败" }, false).detail, "构建失败")
  assert.equal(itemState({ st: "pause" }, false).kind, "paused")
  assert.equal(itemState({ st: "block" }, false).kind, "blocked")
  assert.equal(itemState({ st: "run", q: [{}] }, false).kind, "queued")
  assert.equal(itemState({ st: "run", progress: "等它继续" }, false).kind, "idle")
})

test("the cycle shows as closing without a session, including during delivery verification", () => {
  assert.equal(itemState({ st: "run", phase: "closing" }, false).kind, "closing")
})
