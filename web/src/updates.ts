import { modReq } from "@/api"
import type { Health, Install } from "@/upgrade-guide"
import { t } from "@/i18n"

export type UpdatePhase = "awaiting_approval" | "staging" | "ready" | "draining" | "stopping" | "backing_up" | "restarting" | "verifying" | "committed" | "succeeded" | "rolling_back" | "rolled_back_pending_health" | "rolled_back" | "failed"
export type UpdateCheck = { status: "available" | "up_to_date" | "ahead" | "unknown_current" | "no_release" | "error" | "unsupported_platform"; checked_at: number; version: string | null; release_url: string | null; notes: string | null; error: string | null }
export type UpdateOperation = { id: string; idempotency_key: string; version: string; phase: UpdatePhase; created_at: number; updated_at: number; requested_by: string; approved_by?: string | null; approved_at?: number | null; error: string | null; previous: string | null; target: string | null }
/** `backup.database`: the verified copy the supervisor took before switching, once it exists. */
export type UpdateBackup = { database: string | null; dir: string }
export type UpdateState = { repository: string; current: { hub: string; web: string }; capability: { managed: boolean; can_execute: boolean; reason: string }; check: UpdateCheck | null; operation: UpdateOperation | null; install?: Install; backup?: UpdateBackup | null }
export const updates = {
  status: () => modReq<UpdateState>("GET", "/updates", undefined, AbortSignal.timeout(8000)),
  check: () => modReq<UpdateCheck>("POST", "/updates/check", {}, AbortSignal.timeout(30_000)),
  execute: (version: string, idempotency_key: string) => modReq<UpdateOperation>("POST", "/updates", { version, idempotency_key }, AbortSignal.timeout(30_000)),
  reject: (id: string) => modReq<UpdateOperation>("POST", `/updates/${encodeURIComponent(id)}/reject`, {}, AbortSignal.timeout(30_000)),
  approve: (id: string) => modReq<UpdateOperation>("POST", `/updates/${encodeURIComponent(id)}/approve`, {}, AbortSignal.timeout(30_000)),
  /** The public liveness answer, read directly: it is what a release script waits for after a restart. */
  health: async (): Promise<Health> => {
    const r = await fetch("/healthz", { cache: "no-store", credentials: "same-origin", signal: AbortSignal.timeout(5000) })
    if (!r.ok) throw new Error(`healthz ${r.status}`)
    return r.json()
  },
}
export const updateActive = (op: UpdateOperation | null) => !!op && !["succeeded", "rolled_back", "failed"].includes(op.phase)
// A retry, another tab, or a reload must refer to the same intended version upgrade.
export const updateKey = (repository: string, version: string, retryOf?: string) => `web-upgrade:${repository}:${version}${retryOf ? `:retry:${retryOf}` : ""}`
export const sameReleaseVersion = (left: string, right: string) => left.replace(/^v/, "") === right.replace(/^v/, "")
export const repairKey = (state: UpdateState, version: string) => `${updateKey(state.repository, version)}:align:${state.current.hub}:${state.current.web}:${state.operation?.id ?? "initial"}`
const PHASES: readonly UpdatePhase[] = ["awaiting_approval", "staging", "ready", "draining", "stopping", "backing_up", "restarting", "verifying", "committed", "succeeded", "rolling_back", "rolled_back_pending_health", "rolled_back", "failed"]
/** what each phase reads as, in the current UI language: every lookup translates, so a language switch shows at once */
export const updatePhaseLabel = Object.defineProperties({} as Record<UpdatePhase, string>,
  Object.fromEntries(PHASES.map((p) => [p, { enumerable: true, get: () => t(`misc:update.phase.${p}`) }])))
