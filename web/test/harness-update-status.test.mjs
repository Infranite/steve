import assert from "node:assert/strict"
import { test } from "node:test"
import { receiptStatus, updateTargets } from "../src/harness-update-status.ts"

test("only a final successful receipt reports success", () => {
  for (const state of ["accepted", "running", "waiting"]) assert.equal(receiptStatus({ operation: "op", state: { state } }), "running")
  assert.equal(receiptStatus({ operation: "op", state: { state: "unknown", reason: "connection lost" } }), "unknown")
  for (const outcome of ["failed", "cancelled", "timed_out", "interrupted"]) assert.equal(receiptStatus({ operation: "op", state: { state: "finished", outcome } }), "failed")
  assert.equal(receiptStatus({ operation: "op", state: { state: "finished", outcome: "succeeded" } }), "succeeded")
})

test("one click targets only installed managed components with a checked update", () => {
  const components = [
    { id: "cli", managed: false, state: "available", latest: "3.0.0" },
    { id: "adapter", managed: true, state: "available", latest: "2.0.0" },
    { id: "old", managed: true, state: "current", latest: "1.0.0" },
    { id: "failed", managed: true, state: "check_failed", latest: "9.0.0" },
    { id: "missing", managed: true, state: "not_installed", latest: "2.0.0" },
  ]
  assert.deepEqual(updateTargets({ components }), { adapter: "2.0.0" })
})
