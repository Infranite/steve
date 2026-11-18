/** Shared, pure maintenance states. Acceptance and a network timeout never mean success. */
export type HarnessReceipt = {
  operation: string
  state: { state: "accepted" | "running" | "waiting" } | { state: "unknown"; reason: string }
    | { state: "finished"; outcome: string; error?: { code: string; safe_detail: string } | null }
}
export type ComponentUpdate = {
  id: string; name: string; package?: string | null; managed: boolean
  state: "unchecked" | "current" | "available" | "check_failed" | "unsupported" | "not_installed"
  installed?: string | null; latest?: string | null; checked?: number | null; reason?: string | null
}
export type HarnessUpdate = {
  harness: string; name: string; components: ComponentUpdate[]; can_upgrade: boolean; reason?: string | null; operation?: HarnessReceipt | null
}
export type HarnessUpdates = {
  report: { node: string; checking: boolean; harnesses: HarnessUpdate[] } | null
  at: number; runs: Record<string, HarnessReceipt>; retryable_unknown?: string[]; error?: string | null; code?: string | null
}
export function updateTargets(h: HarnessUpdate): Record<string, string> {
  return Object.fromEntries(h.components.filter(c => c.managed && c.state === "available" && c.latest).map(c => [c.id, c.latest!]))
}
export function receiptStatus(r?: HarnessReceipt | null): "running" | "unknown" | "succeeded" | "failed" | null {
  if (!r) return null
  if (r.state.state === "finished") return r.state.outcome === "succeeded" ? "succeeded" : "failed"
  return r.state.state === "unknown" ? "unknown" : "running"
}
