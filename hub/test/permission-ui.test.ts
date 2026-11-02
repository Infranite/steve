import { test } from "node:test"
import assert from "node:assert/strict"
import type { RDecision } from "../../web/src/api.ts"
import { permissionStatus } from "../../web/src/lib/permission.ts"

const decision = (status: RDecision["status"], result: unknown = null): RDecision => ({
  id: "d-permission", kind: "approve", subject_type: "permission", subject_id: "p-1",
  item_id: null, session_id: "s-1", question: "npm run build", draft: "{}",
  options: ["allow-once|允许一次|allow_once", "reject-once|拒绝一次|reject_once", "reject-always|始终拒绝|reject_always"], status, result, requested_by: "steve",
  created: 1, resolved: status === "open" ? null : 2, undo: null,
})

test("permission records distinguish pending and all terminal outcomes", () => {
  assert.deepEqual(permissionStatus(decision("open")), { label: "待审批", pending: true })
  assert.deepEqual(permissionStatus(decision("resolved", { optionId: "allow-once" })), { label: "已允许", pending: false })
  assert.deepEqual(permissionStatus(decision("resolved", { cancelled: true })), { label: "已拒绝", pending: false })
  assert.deepEqual(permissionStatus(decision("resolved", { optionId: "reject-once" })), { label: "已拒绝", pending: false })
  assert.deepEqual(permissionStatus(decision("resolved", { optionId: "reject-always" })), { label: "已拒绝", pending: false })
  assert.deepEqual(permissionStatus(decision("resolved", { optionId: "allow-once", decided_by: "yolo" })), { label: "已允许", pending: false })
  assert.deepEqual(permissionStatus(decision("expired")), { label: "已失效", pending: false })
  assert.deepEqual(permissionStatus(decision("resolved", { dismissed: true })), { label: "已撤回", pending: false })
})

test("old permission events without a decision never imply approval or pending work", () => {
  assert.deepEqual(permissionStatus(), { label: "历史请求", pending: false })
})
