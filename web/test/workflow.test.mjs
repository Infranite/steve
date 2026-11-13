import assert from "node:assert/strict"
import test from "node:test"
import { orderedUnits, stopPending } from "../src/workflow.ts"

const unit = (id, depends_on = [], status = "pending", required = true) => ({ id, depends_on, status, required })

test("dependency ordering retains independent work and puts review before integration", () => {
  const units = [unit("integrate", ["review"]), unit("review", ["a", "b"]), unit("a"), unit("b")]
  assert.deepEqual(orderedUnits(units).map((u) => u.id), ["a", "b", "review", "integrate"])
  assert.equal(units[0].id, "integrate", "display order never mutates the stored graph")
})

test("malformed historical dependencies remain visible and cannot hang the item page", () => {
  const units = [unit("a", ["b"]), unit("b", ["a"]), unit("missing", ["old"])]
  assert.equal(orderedUnits(units).length, 3)
  assert.equal(orderedUnits(units)[0].id, "missing")
})

test("stop pending shows only on a stopped unit, never on one that succeeded", () => {
  assert.equal(stopPending({ status: "cancelled", stop_pending: true }), true)
  assert.equal(stopPending({ status: "failed", stop_pending: true }), true)
  assert.equal(stopPending({ status: "succeeded", stop_pending: true }), false, "an older Hub flagged finished units whose Node was not attached")
  assert.equal(stopPending({ status: "cancelled", stop_pending: false }), false)
  assert.equal(stopPending({ status: "cancelled" }), false)
})
