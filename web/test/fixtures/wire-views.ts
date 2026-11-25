/* Pure, runtime-dependency-free builders for generated Hub wire view types.
 * Shared by both browser (*.tsx) fixtures and Node (*.browser.mjs / *.ts) fixtures. */
import type {
  ById,
  DecisionView,
  Envelope,
  ItemView,
  MessageView,
  PageOf,
  Patch,
  PatchOp,
  SessionView,
  Snapshot,
  SnapshotViews,
  View,
} from "../../src/gen/index.ts"

export function decisionView(fields: Partial<DecisionView> & Pick<DecisionView, "id">): DecisionView {
  return {
    ws: "default",
    kind: "approve",
    subject_type: "item",
    subject_id: "",
    item_id: null,
    session_id: null,
    question: { kind: "raw", text: "" },
    draft: "",
    options: [],
    status: "open",
    result: null,
    requested_by: "steve",
    created: 0,
    resolved: null,
    undo_until: null,
    space_id: null,
    tier: "decide",
    folded: null,
    revision: 1,
    ...fields,
  }
}

export function itemView(fields: Partial<ItemView> & Pick<ItemView, "id" | "title">): ItemView {
  return {
    ws: "default",
    no: Number(fields.id.split("-")[1]) || 0,
    state: { kind: "active" },
    cycle: 1,
    revision: 1,
    delivery_revision: 0,
    delivery: [],
    waits: [],
    priority: "中",
    star: false,
    archived: false,
    design_gate: false,
    source: "",
    created: 0,
    updated: 0,
    goals: 0,
    tags: [],
    deps: [],
    plan_total: 0,
    plan_done: 0,
    op_blockers: [],
    active_operations: 0,
    detail_stamp: 0,
    has_branch: false,
    ...fields,
  }
}

export function sessionView(fields: Partial<SessionView> & Pick<SessionView, "id">): SessionView {
  return {
    ws: "default",
    no: 1,
    cwd: "",
    title: "",
    status: "idle",
    created: 0,
    updated: 0,
    readonly: false,
    busy: false,
    queued: 0,
    ...fields,
  }
}

export function messageView(fields: Partial<MessageView> & Pick<MessageView, "id" | "author" | "text">): MessageView {
  return {
    ws: "default",
    item_id: null,
    created: 0,
    meta: null,
    ...fields,
  }
}

export function snapshotViews(
  views: SnapshotViews,
  opts: { epoch?: string; rev?: number; cursor?: string } = {},
): Snapshot {
  const epoch = opts.epoch ?? "fixture"
  const rev = opts.rev ?? 1
  return { epoch, rev, cursor: opts.cursor ?? `${epoch}:${rev}`, views }
}

export function emptySnapshot(opts: { epoch?: string; rev?: number; cursor?: string; withMessage?: boolean } = {}): Snapshot {
  return snapshotViews(
    opts.withMessage
      ? { item: [], session: [], decision: [], message: { rev: opts.rev ?? 1, views: [], next: null } }
      : { item: [], session: [], decision: [] },
    opts,
  )
}

export function viewsResponse(views: View[], rev = 1, missing: string[] = []): ById {
  return { rev, views, missing }
}

export function pageResponse<V>(views: V[], rev = 0, next: string | null = null): PageOf<V> {
  return { rev, views, next }
}

export function patchFrame(ops: PatchOp[], rev: number, ws = "default"): Envelope<Patch> {
  return { type: "patch", ws, rev, ops }
}
