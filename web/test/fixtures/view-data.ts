import { applyPatch, qc, scopeKey } from "../../src/data/sync"
import type { Item, Msg, Sess, Wait } from "../../src/store"
import type { DecisionView, ItemView, MessageView, PatchOp, SessionView } from "../../src/gen"
import { api, type ConversationMessage } from "../../src/api"

let rev = 0
function patch(ops: PatchOp[]) {
  applyPatch({ type: "patch", ws: "default", rev: ++rev, ops })
}

export function decisionFixture(w: Wait): DecisionView {
  return {
    id: w.decision, ws: "default", kind: w.type, subject_type: w.subject,
    subject_id: w.subjectId ?? "", item_id: null, session_id: w.session,
    question: { kind: "raw", text: w.q }, draft: w.draft ?? "", options: w.options, status: "open", requested_by: w.by,
    created: 1, result: w.recovery ? { recovery: w.recovery } : null,
    resolved: null, undo_until: null, space_id: w.space ?? null, tier: w.tier ?? "decide", folded: w.folded ?? null, revision: 1,
  }
}

export function seedDecisions(waits: Wait[]) {
  patch(waits.map((w): PatchOp => ({ type: "decision", op: "upsert", id: w.decision, view: decisionFixture(w) })))
}

/** The typed lifecycle a fixture's display status stands for (a waiting item is active with an open decision). */
function fixtureState(st: Item["st"], phase?: Item["phase"]): ItemView["state"] {
  if (phase === "closed" && st === "run") return { kind: "closed", outcome: "failed", by: "acceptance" }
  switch (st) {
    case "done": return { kind: "closed", outcome: "finished", by: "owner" }
    case "drop": return { kind: "closed", outcome: "cancelled", by: "owner" }
    case "fail": return { kind: "closed", outcome: "failed", by: "acceptance" }
    case "pause": return { kind: "paused" }
    case "prop": return { kind: "ready" }
    default: return { kind: "active" }
  }
}

export function seedItems(items: (Pick<Item, "id" | "t" | "st" | "b"> & Partial<Item>)[]) {
  const ops: PatchOp[] = []
  for (const i of items) {
    const parent = typeof i.parent === "object" ? i.parent : null
    const view: ItemView = {
      id: i.id, ws: "default", no: Number(i.id.split("-")[1]) || 0, title: i.t, bot_id: i.b,
      state: fixtureState(i.st, i.phase), cycle: i.cycle ?? 1, revision: 1, delivery_revision: 0, delivery: [],
      waits: i.st === "block" ? ["prerequisite"] : [],
      priority: i.pri ?? "中", star: !!i.star, archived: !!i.arch, design_gate: !!i.designGate,
      parent: parent ? { item: parent.item, session: parent.s, at: parent.at } : undefined,
      accept_mode: i.acc, project_id: i.project ?? undefined, source: i.src?.[1] ?? "",
      created: i.created ?? 0, updated: i.updated ?? 0,
      goals: i.goals?.length ?? 0,
      tags: i.tags ?? [], deps: i.deps ?? [], plan_total: i.plan?.length ?? 0,
      plan_done: i.plan?.filter((p) => p[1] === 1).length ?? 0, op_blockers: [], active_operations: 0,
      detail_stamp: 0, has_branch: !!i.branch,
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
        question: { kind: "raw", text: w.q ?? "" }, draft: w.draft ?? "", options: w.options ?? [],
        status: "open", requested_by: w.by ?? i.b, created: 0, result: null,
        resolved: null, undo_until: null, space_id: null, tier: w.tier ?? "decide", folded: null, revision: 1,
      } })
    }
    if (i.workflow || i.outs?.length || i.progress) {
      qc.setQueryData(["detail", scopeKey(), "item", i.id], {
        workflow: i.workflow, decisions: [], timeline: i.progress ? [{ id: 1, ts: i.updated ?? 0, actor: i.b, kind: "progress", text: i.progress, session_id: null }] : [], dependents: [], goalsList: [], plan: [], planRounds: [],
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
      used: s.ctxUsed, size: s.ctxSize,
      created: s.since ?? 0, updated: s.at ?? 0, busy: !!s.busy, queued: s.queued ?? s.queue?.length ?? 0,
      waiting: s.room ?? undefined,
    } satisfies SessionView,
  })))
}

// These fixtures render components directly and seed all their rows above.
api.itemSessions = async () => ({ rev, views: [], missing: [] })


let seededMessages = new Set<string>()
export function seedMessages(messages: (Msg | ConversationMessage)[], replace = true) {
  const ops: PatchOp[] = []
  const next = new Set<string>()
  for (const input of messages) {
    if (!input.id) continue
    const view: MessageView = "author" in input ? { ...input, ws: "default" } : {
      id: input.id, ws: "default", author: input.k, text: input.t ?? "",
      item_id: input.item ?? null, created: input.created ?? 0,
      meta: { ...input, reply_to: input.replyTo, card_only: input.cardOnly, was_card: input.wasCard, a2ui_action: input.a2uiAction },
    }
    ops.push({ type: "message", op: "upsert", id: view.id, view })
    next.add(view.id)
  }
  if (replace) {
    for (const id of seededMessages) if (!next.has(id)) ops.push({ type: "message", op: "remove", id })
    seededMessages = next
  }
  patch(ops)
}
