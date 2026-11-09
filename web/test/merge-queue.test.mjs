import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/merge-queue-model.ts")
const field = (k) => m.POLICY_FIELDS.find((f) => f.key === k)

test("every number of the queue's policy has a field, minutes shown for seconds", () => {
  const keys = m.POLICY_FIELDS.map((f) => f.key)
  for (const k of ["max_batch", "settle_s", "checks", "check_deadline_s", "host_deadline_s", "release_window_s", "release_max", "release_idle_s", "node_wait_s", "yolo_reruns", "paused", "cut_ci_wait_s", "cut_deadline_s", "moved_retries", "host_poll_s"]) assert.ok(keys.includes(k), k)
  assert.equal(m.shown(field("release_window_s"), 7200), "120")
  assert.equal(m.shown(field("settle_s"), 30), "0.5")
  assert.equal(m.shown(field("checks"), ["make test", "make lint"]), "make test\nmake lint")
  assert.equal(m.shown(field("release_idle_s"), null), "")
})

test("an input reads back as the field's value; blank is the default, a bad one is refused", () => {
  assert.equal(m.read(field("release_window_s"), "90"), 5400)
  assert.equal(m.read(field("max_batch"), "3"), 3)
  assert.equal(m.read(field("max_batch"), "2.5"), undefined)
  assert.equal(m.read(field("max_batch"), "-1"), undefined)
  assert.equal(m.read(field("max_batch"), " "), null)
  assert.deepEqual(m.read(field("checks"), "make test\n\n make lint "), ["make test", "make lint"])
  assert.equal(m.read(field("checks"), ""), null)
  assert.equal(m.read(field("paused"), "1"), true)
  assert.equal(m.read(field("paused"), ""), false)
})

test("only what changed goes into the patch, and nothing when a value does not read", () => {
  const set = { max_batch: 3 }
  assert.deepEqual(m.patchOf(set, { max_batch: "3", release_max: "5" }), { release_max: 5 })
  assert.deepEqual(m.patchOf(set, { max_batch: "" }), { max_batch: null })
  assert.equal(m.patchOf(set, { max_batch: "x" }), undefined)
})

test("live batches first, a bisection's halves under the batch they split from", () => {
  const b = (no, state, parent = null, root = no) => ({ no, parent, root, state, step: null, members: [], others: 0, outcome: null, reruns: 0, base: null, tip: null, opened: 0, ended: null })
  const got = m.ordered([b(1, "landed"), b(2, "split"), b(3, "landed", 2, 2), b(4, "running", 2, 2), b(5, "split", null, 5), b(6, "running", 5, 5), b(7, "waiting", 5, 5)])
  assert.deepEqual(got.map((x) => [x.b.no, x.depth]), [[5, 0], [6, 1], [7, 1], [2, 0], [3, 1], [4, 1], [1, 0]])
})
