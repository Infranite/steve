import assert from "node:assert/strict"
import test from "node:test"
import { coverageValue } from "../src/stats/usage-coverage.ts"

const labels = { not_reported: "尚未上报", unavailable: "用量不可用", partial: "部分覆盖" }
const text = (state, total, field = "total") => coverageValue({ total: 0, coverage: { state, total, input: total, output: null, cache_read: null, cache_write: null } }, field, String, labels)

test("unreported/unavailable never format as 0, while an observed zero does", () => {
  assert.equal(text("not_reported", null), "尚未上报")
  assert.equal(text("unavailable", null), "用量不可用")
  assert.equal(text("complete", 0), "0")
  assert.equal(text("partial", 0), "≥0 · 部分覆盖")
  assert.equal(text("partial", 12), "≥12 · 部分覆盖")
  assert.equal(text("partial", 12, "output"), "部分覆盖")
})
