import { modReq, qs } from "@/api"

export type DebugCapture = {
  active: boolean
  started_at: number | null
  expires_at: number | null
  stop_reason: string | null
  bytes: number
  dropped: number
}
export type DebugCoverage = {
  earliest_cursor?: number | null
  latest_cursor?: number | null
  retention_days?: number
  limits?: Record<string, unknown>
  instance_pending?: number
  pending_scope?: "instance"
  recorder_available?: boolean
  delivery?: "best_effort"
  unobserved_gaps?: "unknown"
  loss_count_stable?: boolean
  refresh_required?: boolean
}
export type DebugSnapshot = {
  observed_at: number
  item: { id: string; status: string; phase: string; cycle: number; revision: number }
  capture: DebugCapture
  sessions: { id: string; status?: string; harness?: string }[]
  coverage: DebugCoverage
  state?: {
    observed_at?: number
    lifecycle?: { phase?: string; blockers?: { kind: string; label: string }[]; closing?: { waiting?: unknown[] } | null } | null
    dependencies?: { open_dependencies?: string[]; resume_mode?: string; resume_source?: string; resume_enabled?: boolean; resume_blocker?: string | null } | null
    lane?: { state?: string; summary?: string; advice?: string } | null
    changed_during_read?: boolean
  }
  can_capture: boolean
}
export type DebugEvent = {
  event_id: string
  at: number
  cursor: number
  kind: string
  reason_code: string
  severity: string
  session_id?: string | null
  attempt_id?: string | null
  operation_id?: string | null
  decision_id?: string | null
  queued_id?: string | null
  cause_id?: string | null
  details: Record<string, unknown>
}
export type DebugPage = {
  events: DebugEvent[]
  next_cursor: string | null
  has_more: boolean
  coverage: DebugCoverage
  gaps: unknown[]
  truncated: boolean
}
export type DebugFilter = {
  kinds?: string
  severity?: string
  session_id?: string
  attempt_id?: string
  since?: number
  until?: number
}

/** The cursor is opaque and includes the first page's snapshot boundary. */
export const debugApi = {
  explain: (item: string, signal?: AbortSignal) => modReq<DebugSnapshot>("GET", `/items/${encodeURIComponent(item)}/debug`, undefined, signal ? AbortSignal.any([signal, AbortSignal.timeout(30_000)]) : undefined),
  events: (item: string, filter: DebugFilter, cursor?: string, signal?: AbortSignal) => modReq<DebugPage>("GET", `/items/${encodeURIComponent(item)}/debug/events${qs({ ...filter, cursor, limit: 50 })}`, undefined, signal ? AbortSignal.any([signal, AbortSignal.timeout(30_000)]) : undefined),
  capture: (item: string, enabled: boolean, minutes = 30) => modReq<DebugSnapshot>("POST", `/items/${encodeURIComponent(item)}/debug`, enabled ? { enabled, minutes } : { enabled }),
}
