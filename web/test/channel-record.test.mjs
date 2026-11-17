import assert from "node:assert/strict"
import { test } from "node:test"
import { handlingDetail, shownUpTo, weave } from "../src/thread/channel-record.ts"

const rec = (id, created, direction = "in", turn = null, handling) => ({ id, created, direction, turn, handling })
const msg = (id, created) => ({ id, created })

test("messages without a turn come in between the turns by time", () => {
  const msgs = [msg("t1", 20), msg("s1", 25), msg("t2", 40)]
  const records = [rec("c3", 30), rec("c1", 10), rec("c5", 50), rec("c2", 20)]
  const { entries } = weave(msgs, records)
  assert.deepEqual(entries.map((e) => (e.k === "msg" ? e.m.id : e.r.id)), ["c1", "c2", "t1", "s1", "c3", "t2", "c5"])
})

test("a message shown by its turn is not shown twice: received ones lend the turn their handling", () => {
  const handling = { state: "failed", label: "", reason: "", error: "超时", item: null, decision: null }
  const msgs = [msg("t1", 20), msg("s1", 25)]
  const records = [rec("in1", 19, "in", "t1", handling), rec("out1", 26, "out", "s1"), rec("in2", 30)]
  const { entries, byTurn } = weave(msgs, records)
  assert.deepEqual(entries.map((e) => (e.k === "msg" ? e.m.id : e.r.id)), ["t1", "s1", "in2"])
  assert.equal(byTurn.get("t1").handling.error, "超时")
  assert.equal(byTurn.has("s1"), false)
})

test("a turn not loaded leaves its message in the record", () => {
  const { entries, byTurn } = weave([msg("t9", 5)], [rec("in1", 19, "in", "t1")])
  assert.deepEqual(entries.map((e) => (e.k === "msg" ? e.m.id : e.r.id)), ["t9", "in1"])
  assert.equal(byTurn.size, 0)
})

test("a message still being written keeps its place", () => {
  const { entries } = weave([msg("t1", 10), { id: undefined, created: undefined }], [rec("c1", 20)])
  assert.deepEqual(entries.map((e) => (e.k === "msg" ? e.m.id ?? "draft" : e.r.id)), ["t1", "draft", "c1"])
})

test("how far you have read is the newest message shown", () => {
  assert.equal(shownUpTo([msg("a", 5), { id: "b" }], [rec("c", 9)]), 9)
  assert.equal(shownUpTo([], []), 0)
})

test("a failure says why; a skip what it was recorded with; an answer nothing more", () => {
  const h = (state, more = {}) => ({ state, label: "", reason: "", error: null, ...more })
  assert.equal(handlingDetail(h("failed", { error: "飞书暂时不可用", reason: "规则" })), "飞书暂时不可用")
  assert.equal(handlingDetail(h("failed", { reason: "起草器：超时" })), "起草器：超时")
  assert.equal(handlingDetail(h("skipped", { label: "不需要回复", reason: "起草器：只是寒暄" })), "不需要回复 · 起草器：只是寒暄")
  assert.equal(handlingDetail(h("answered", { label: "已回复" })), "")
})
