import { applyPatch, qc, scopeKey } from "../../src/data/sync"
import type { Item, Sess } from "../../src/store"
import type { ItemView, PatchOp, SessionView } from "../../src/gen"
import { api } from "../../src/api"

let rev = 0
function patch(ops: PatchOp[]) {
  applyPatch({ type: "patch", ws: "default", rev: ++rev, ops })
}

export function seedItems(items: Item[]) {
  const ops: PatchOp[] = []
  for (const i of items) {
    const parent = typeof i.parent === "object" ? i.parent : null
    const view: ItemView = {
      id: i.id, ws: "default", no: Number(i.id.split("-")[1]) || 0, title: i.t, bot_id: i.b,
      status: i.st, phase: i.phase ?? "active", stage: "执行", cycle: i.cycle ?? 1, revision: 0,
      priority: i.pri ?? "中", star: !!i.star, archived: !!i.arch, design_gate: !!i.designGate,
      parent: parent ? { item: parent.item, session: parent.s, at: parent.at } : undefined,
      accept_mode: i.acc, project_id: i.project ?? undefined, source: i.src ?? ["", ""],
      now: i.now ?? "", created: i.created ?? 0, updated: i.updated ?? 0,
      goal: i.goals?.at(-1) ?? "", goals: i.goals?.length ?? 0, closeout: i.closeout,
      tags: i.tags ?? [], deps: i.deps ?? [], plan_total: i.plan?.length ?? 0,
      plan_done: i.plan?.filter((p) => p[1] === 1).length ?? 0, op_blockers: [], active_operations: 0,
      detail_stamp: "fixture",
      workflow: i.workflowCycle != null || i.workflowPaused || i.workflowAttempt ? {
        paused: i.workflowPaused ?? null, cycle: i.workflowCycle ?? null,
        failed: i.workflowFailed ?? [], attempt: i.workflowAttempt ?? null, superseded: !!i.workflowSuperseded,
      } : undefined,
    }
    ops.push({ type: "item", op: "upsert", id: i.id, view })
    for (const [n, w] of (i.waits ?? []).entries()) {
      const id = w.decision ?? `fixture:${i.id}:${n}`
      ops.push({ type: "decision", op: "upsert", id, view: {
        id, ws: "default", kind: w.type ?? "ask", item_id: i.id, session_id: w.session ?? null,
        subject_type: w.subject ?? "item", subject_id: w.subjectId ?? i.id,
        question: w.q ?? "", draft: w.draft ?? "", options: w.options ?? [],
        status: "open", requested_by: w.by ?? i.b, created: 0, result: null,
        resolved: null, undo_until: null, space_id: null, tier: w.tier ?? "decide", folded: null,
      } })
    }
    if (i.workflow || i.outs?.length) {
      qc.setQueryData(["detail", scopeKey(), "item", i.id], {
        workflow: i.workflow, decisions: [], timeline: [], dependents: [], goalsList: [],
        artifacts: (i.outs ?? []).map(([kind, title, location, id, created, , delivery_id]) => ({ kind, title, location, id, created, delivery_id })),
      })
    }
  }
  patch(ops)
}

export function seedSessions(sessions: Record<string, Partial<Sess>>) {
  patch(Object.entries(sessions).map(([id, s]): PatchOp => ({
    type: "session", op: "upsert", id,
    view: {
      id, ws: "default", item_id: s.item ?? undefined, no: s.n ?? 1,
      bot_id: s.b ?? undefined, harness: s.harness ?? s.h, model: s.m, node_id: s.node,
      cwd: s.cwd ?? "", device_id: s.device, title: s.title ?? "", source: s.src ?? "",
      readonly: !!s.ro, status: s.st === "run" ? "running" : s.st === "ended" ? "ended" : "idle",
      created: s.since ?? 0, updated: s.at ?? 0, busy: !!s.busy, queued: s.queued ?? s.queue?.length ?? 0,
      waiting: s.room ?? undefined,
    } satisfies SessionView,
  })))
}

// These fixtures render components directly and seed all their rows above.
api.itemSessions = async () => ({ rev, views: [], missing: [] })
