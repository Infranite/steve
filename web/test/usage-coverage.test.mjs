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
test("an older Hub's unsupported zero remains unknown", () => {
  assert.equal(coverageValue({ total: 0 }, "total", String, labels), "尚未上报")
  assert.equal(coverageValue({ total: 9 }, "total", String, labels), "9")
})

test("legacy cost ownership uncertainty is explicit without a fabricated unavailable call", () => {
  const coverage = { state: "partial", uncertain: 1, total: 15, input: 10, output: 5, cache_read: 0, cache_write: 0 }
  assert.equal(coverageValue({ total: 15, coverage }, "total", String, { ...labels, uncertain: "历史费用归属不确定" }), "≥15 · 历史费用归属不确定")
  assert.equal(coverageValue({ total: 15, coverage }, "total", String, { ...labels, uncertain: "Historical cost ownership uncertain" }), "≥15 · Historical cost ownership uncertain")
})
