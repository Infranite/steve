/* Workspace approval policy and per-session effective approval state. */
import { modReq } from "@/api"
import type { ApprovalMode, ApprovalPolicy } from "@/gen"

export type ApprovalPolicyView = { policy: ApprovalPolicy; editable: boolean }
export type SessionApprovalMode = {
  status: "on" | "off" | "unsupported"
  applied: string | null
  desired: string | null
  refused: string | null
  target: string | null
  native_id: string | null
  unknown: boolean
  released: boolean
}

export const approvalApi = {
  getPolicy: (signal?: AbortSignal) => modReq<ApprovalPolicyView>("GET", "/approval-policy", undefined, signal),
  setPolicy: (mode: ApprovalMode, revision: number) => modReq<ApprovalPolicyView>("PUT", "/approval-policy", { mode, revision }),
  sessionMode: (id: string, signal?: AbortSignal) => modReq<SessionApprovalMode>("GET", `/approval-policy/sessions/${encodeURIComponent(id)}`, undefined, signal),
}
