/* Workspace permission policy and per-session effective permission state. */
import { modReq } from "@/api"
import type { ApprovalMode, PermissionPolicyView, SessionPermissionView } from "@/gen"

export type { ApprovalMode, PermissionPolicyView, SessionPermissionView }

export const approvalApi = {
  getPolicy: (signal?: AbortSignal) => modReq<PermissionPolicyView>("GET", "/permission-policy", undefined, signal),
  setPolicy: (mode: ApprovalMode, revision: number, confirmed = false) =>
    modReq<PermissionPolicyView>("PUT", "/permission-policy", { mode, revision, ...(confirmed ? { confirmed: true } : {}) }),
  sessionMode: (id: string, signal?: AbortSignal) =>
    modReq<SessionPermissionView>("GET", `/permission-policy/sessions/${encodeURIComponent(id)}`, undefined, signal),
}
