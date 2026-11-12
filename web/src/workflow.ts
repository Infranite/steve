/** A work graph uses explicit unit IDs; its display order follows dependencies, not arrival order. */
export type WorkflowUnit = {
  id: string; title: string; goal: string; bot_id: string; kind: "work" | "review" | "integrate"
  depends_on: string[]; resources: string[]; required: boolean; status: string
  item_id?: string | null; session_id?: string | null; attempt: number
  summary?: string; reason?: string | null
  outputs?: { kind: string; title: string; location?: string | null; role?: string | null }[]
  checks?: { name: string; passed: boolean; operated?: boolean }[]
}
export type Workflow = {
  item_id: string; revision: number; paused: boolean; max_parallel: number; max_attempts: number
  units: WorkflowUnit[]; blockers: string[]; execution_ready?: boolean
  current_cycle?: boolean; history_total?: number
  acceptance_mode?: "collaborative" | "single_session"
  history: { revision: number; reason: string; created: number; action?: string; unit_id?: string | null; detail?: unknown }[]
}

export function orderedUnits(units: WorkflowUnit[]): WorkflowUnit[] {
  const pending = [...units], out: WorkflowUnit[] = [], seen = new Set<string>()
  const ids = new Set(units.map((u) => u.id))
  while (pending.length) {
    const at = pending.findIndex((u) => u.depends_on.every((id) => seen.has(id) || !ids.has(id)))
    // The server rejects cycles. Keep a malformed / old graph visible instead of hanging the page.
    const [unit] = pending.splice(at < 0 ? 0 : at, 1)
    out.push(unit)
    seen.add(unit.id)
  }
  return out
}

export function workflowProgress(workflow: Workflow) {
  const required = workflow.units.filter((u) => u.required)
  return {
    passed: required.filter((u) => u.status === "succeeded").length,
    total: required.length,
    running: workflow.units.filter((u) => u.status === "running" || u.status === "dispatching").length,
  }
}
