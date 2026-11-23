import assert from "node:assert/strict"
import { test } from "node:test"
import { recoveredCalls, researchEvent } from "../src/thread/chat-progress.ts"

const raw = { type: "steve.tool", mid: "msg-1", run: "run-1", ws: "test", name: "Read", kind: "read", brief: "notes.md", ok: true }
const event = (l, changes) => researchEvent(l, { ...raw, ...changes })

test("a running tool is not a success and a different call's completion cannot clear it", () => {
  let live = event(null, { call: 1, running: true, started_at: 100, at: 1000 })
  live = event(live, { call: 2, running: true, at: 1010 })
  assert.equal(live.tools.length, 0)
  assert.equal(live.now.call, 2)
  live = event(live, { call: 2, ok: false, at: 1020 })
  assert.equal(live.now.call, 1)
  assert.equal(live.startedAt, 100)
  assert.equal(live.at, 1020)
  assert.equal(live.tools[0].ok, false)
})

test("recovery requires a success on the same stable call; legacy rows remain failures", () => {
  let live = event(null, { call: 1, ok: false })
  live = event(live, { call: 2, ok: true })
  live = event(live, { ok: false })
  assert.deepEqual([...recoveredCalls(live.tools, row => row.ok)], [])
  live = event(live, { call: 1, running: true })
  assert.deepEqual([...recoveredCalls(live.tools, row => row.ok)], [])
  live = event(live, { call: 1 })
  live = event(live, { call: 1 })
  assert.equal(live.tools.length, 4, "same terminal view is not duplicated")
  assert.deepEqual([...recoveredCalls(live.tools, row => row.ok)], [0])
  live = event(live, { run: "run-2", call: 1 })
  assert.equal(live.tools.length, 1)
  assert.deepEqual([...recoveredCalls(live.tools, row => row.ok)], [])
  assert.equal(live.startedAt, undefined)
  assert.equal(live.at, undefined)
})


test("tool changes leave the last published snapshot intact for frame-batched readers", () => {
  const before = event(null, { call: 1, running: true })
  const after = event(before, { call: 1 })
  assert.equal(before.now.call, 1)
  assert.deepEqual(before.tools, [])
  assert.equal(after.now, undefined)
  assert.equal(after.tools[0].call, 1)
})
