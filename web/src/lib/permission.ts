import type { RDecision } from "../api"

/** Missing old decisions are historical records, never inferred approvals or pending requests. */
export function permissionStatus(d?: RDecision) {
  if (!d) return { label: "历史请求", pending: false }
  if (d.status === "open") return { label: "待审批", pending: true }
  if (d.status === "expired") return { label: "已失效", pending: false }
  if (d.result?.dismissed) return { label: "已撤回", pending: false }
  const option = d.options.find((o) => o.split("|")[0] === d.result?.optionId)
  const rejected = option?.split("|")[2]?.startsWith("reject")
  return { label: d.result?.optionId && !rejected ? "已允许" : "已拒绝", pending: false }
}
