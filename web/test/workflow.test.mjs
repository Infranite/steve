import assert from "node:assert/strict"
import test from "node:test"
import { orderedUnits } from "../src/workflow.ts"

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
