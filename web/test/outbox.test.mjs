import assert from "node:assert/strict"
import { test } from "node:test"
import { deadSub, reviveText } from "../src/outbox.ts"

const group = (o = {}) => ({ consumer: "github.after", topic: "github.after", count: 3, last_error: null, updated: 0, ...o })

test("a dead group says how many and why, with a long error cut", () => {
  assert.equal(deadSub(group()), "3 条没能送达，已停止自动重试")
  assert.equal(deadSub(group({ last_error: "connection\n reset" })), "3 条没能送达，已停止自动重试 · connection reset")
  const long = deadSub(group({ last_error: "x".repeat(500) }))
  assert.ok(long.endsWith("…") && long.length < 220, long)
})

test("the revive toast counts what was sent again", () => {
  assert.equal(reviveText(2), "已重新投递 2 条")
  assert.equal(reviveText(0), "没有需要重新投递的")
})
