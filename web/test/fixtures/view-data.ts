import { applyPatch, qc, scopeKey } from "../../src/data/sync"
import type { Bot, Item, Msg, Sess, Wait } from "../../src/store"
import type { DecisionView, ItemView, MessageView, PatchOp, SessionView } from "../../src/gen"
import { api, type ConversationMessage, type DecisionAnswer, type RBot, type RItemDetail, type RProject, type RRuntime, type RWorkspace, type Shell } from "../../src/api"
import type { UsageCoverage } from "../../src/api/stats"
import { decisionView, itemView, messageView, sessionView } from "./wire-views.ts"

let rev = 0
function patch(ops: PatchOp[]) {
  applyPatch({ type: "patch", ws: "default", rev: ++rev, ops })
}

export function fixtureWorkspace(overrides: Partial<RWorkspace> = {}): RWorkspace {
  return { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0, ...overrides }
}

export function fixtureBot(overrides: Partial<Bot> = {}): Bot {
  return {
    id: "dev", n: "Steve 开发", a: "", role: "", pers: "", pro: 1, h: "Codex", harness: "codex",
    m: "", model: null, on: true, acc: "manual", ws: "default", project: null, perm: "", permMode: "auto",
    rule: "", prompt: null, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: ["default"],
    ...overrides,
  }
}

export function fixtureRawBot(overrides: Partial<RBot> = {}): RBot {
  return {
    id: "dev", name: "开发 Bot", role: "", main: false, harness: "claude-code", model: null,
    rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {},
    default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"],
    ...overrides,
  }
}

export function fixtureProject(overrides: Partial<RProject> = {}): RProject {
  return {
    id: "p1", workspace_id: "default", name: "Steve", description: "", device_id: "hub", path: "/tmp/steve",
    kind: "git", worktree: true, default_bot: null, release: null, archived: false, created: 0, updated: 0,
    ...overrides,
  }
}

export function fixtureRuntime(overrides: Partial<RRuntime> & Pick<RRuntime, "id" | "name">): RRuntime {
  return { ok: true, version: "", path: "", models: [], note: "", ...overrides }
}

export function fixtureShell(overrides: Partial<Shell> = {}): Shell {
  return {
    modules: { feishuRegistration: false },
    version: "test",
    web_version: "test",
    workspace: fixtureWorkspace(),
    workspaces: [],
    bots: [],
    allBots: [],
    projects: [],
    tags: [],
    settings: {},
    ...overrides,
  }
}

export function fixtureItem(overrides: Partial<Item> & Pick<Item, "id" | "t">): Item {
  return {
    b: "dev", st: "run", src: ["", ""], ws: "default", project: null, up: "", acc: "inherit",
    goals: [], progress: "", rounds: [], waits: [], ss: [], tl: [], outs: [], anns: [], q: [], qs: null,
    branch: null, cwd: null, detail: false, dependents: [], created: 0, updated: 0,
    ...overrides,
  }
}

export function fixtureItemDetail(overrides: Partial<RItemDetail> & Pick<RItemDetail, "id">): RItemDetail {
  return {
    plan: [], goalsList: [], timeline: [], artifacts: [], decisions: [], dependents: [],
    ...overrides,
  }
}

export function fixtureWait(overrides: Partial<Wait> & Pick<Wait, "q" | "decision">): Wait {
  return {
    type: "approve", since: "刚刚", subject: "item", session: null, options: ["yes", "no"], by: "dev", tier: "decide",
    ...overrides,
  }
}

export function fixtureSess(overrides: Partial<Sess> & Pick<Sess, "id">): Sess {
  return {
    n: 1, item: null, b: "dev", st: "idle", title: "", dur: "", ctx: 0, src: "", up: "",
    queue: [], device: "hub", queued: 0, busy: false,
    ...overrides,
  }
}

export function fixtureDecisionAnswer(overrides: Partial<DecisionAnswer> & Pick<DecisionAnswer, "id">): DecisionAnswer {
  return { revision: 1, status: "resolved", kind: "approve", subject_type: "item", ...overrides }
}

export function fixtureCoverage(overrides: Partial<UsageCoverage> = {}): UsageCoverage {
  return {
    state: "complete", expected: 1, reported: 1, complete: 1, pending: 0, unavailable: 0,
    total: 0, input: 0, output: 0, cache_read: 0, cache_write: 0, cost_reported: null,
    ...overrides,
  }
}

export function decisionFixture(w: Wait): DecisionView {
  return decisionView({
    id: w.decision,
    kind: w.type,
    subject_type: w.subject,
    subject_id: w.subjectId ?? "",
    session_id: w.session,
    question: { kind: "raw", text: w.q },
    draft: w.draft ?? "",
    options: w.options,
    requested_by: w.by,
    created: 1,
    result: w.recovery ? { recovery: w.recovery } : null,
    space_id: w.space ?? null,
    tier: w.tier ?? "decide",
    folded: w.folded ?? null,
  })
}

export function seedDecisions(waits: Wait[]) {
  patch(waits.map((w): PatchOp => ({ type: "decision", op: "upsert", id: w.decision, view: decisionFixture(w) })))
}

export function seedDecisionViews(views: DecisionView[]) {
  patch(views.map((view): PatchOp => ({ type: "decision", op: "upsert", id: view.id, view })))
}

/** The typed lifecycle a fixture's display status stands for (a waiting item is active with an open decision). */
function fixtureState(st: Item["st"], phase?: Item["phase"]): ItemView["state"] {
  switch (st) {
    case "done": return { kind: "closed", outcome: "finished", by: "owner" }
    case "drop": return { kind: "closed", outcome: "cancelled", by: "owner" }
    case "fail": return { kind: "closed", outcome: "failed", by: "acceptance" }
    case "pause": return { kind: "paused" }
    case "prop": return { kind: "ready" }
    default: return phase === "closed" ? { kind: "closed", outcome: "failed", by: "acceptance" } : { kind: "active" }
  }
}

export function seedItems(items: (Pick<Item, "id" | "t" | "st" | "b"> & Partial<Item>)[]) {
  const ops: PatchOp[] = []
  for (const i of items) {
    const parent = typeof i.parent === "object" ? i.parent : null
    const view: ItemView = itemView({
      id: i.id, title: i.t, bot_id: i.b,
      state: fixtureState(i.st, i.phase), cycle: i.cycle ?? 1,
      waits: i.st === "block" ? ["prerequisite"] : [],
      priority: i.pri ?? "中", star: !!i.star, archived: !!i.arch, design_gate: !!i.designGate,
      parent: parent ? { item: parent.item, session: parent.s, at: parent.at } : undefined,
      accept_mode: i.acc, project_id: i.project ?? undefined, source: i.src?.[1] ?? "",
      created: i.created ?? 0, updated: i.updated ?? 0,
      goals: i.goals?.length ?? 0,
      tags: i.tags ?? [], deps: i.deps ?? [], plan_total: i.plan?.length ?? 0,
      plan_done: i.plan?.filter((p) => p[1] === 1).length ?? 0,
      has_branch: !!i.branch,
    })
    ops.push({ type: "item", op: "upsert", id: i.id, view })
    for (const [n, w] of (i.waits ?? []).entries()) {
      const id = w.decision ?? `fixture:${i.id}:${n}`
      ops.push({ type: "decision", op: "upsert", id, view: decisionView({
        id, kind: w.type ?? "ask", item_id: i.id, session_id: w.session ?? null,
        subject_type: w.subject ?? "item", subject_id: w.subjectId ?? i.id,
        question: { kind: "raw", text: w.q ?? "" }, draft: w.draft ?? "", options: w.options ?? [],
        requested_by: w.by ?? i.b, tier: w.tier ?? "decide",
      }) })
    }
    if (i.outs?.length || i.progress) {
      qc.setQueryData(["detail", scopeKey(), "item", i.id], {
        decisions: [], timeline: i.progress ? [{ id: 1, ts: i.updated ?? 0, actor: i.b, kind: "progress", text: i.progress, session_id: null }] : [], dependents: [], goalsList: [], plan: [], planRounds: [],
        artifacts: (i.outs ?? []).map(([kind, title, location, id, created, , delivery_id]) => ({ kind, title, location, id, created, delivery_id })),
      })
    }
  }
  patch(ops)
}

export function seedItemViews(views: ItemView[]) {
  patch(views.map((view): PatchOp => ({ type: "item", op: "upsert", id: view.id, view })))
}

export function seedSessions(sessions: Record<string, Partial<Sess>>) {
  patch(Object.entries(sessions).map(([id, s]): PatchOp => ({
    type: "session", op: "upsert", id,
    view: sessionView({
      id, item_id: s.item ?? undefined, no: s.n ?? 1,
      bot_id: s.b ?? undefined, harness: s.harness ?? s.h, model: s.m, node_id: s.node,
      cwd: s.cwd ?? "", device_id: s.device, title: s.title ?? "", source: s.src ?? "",
      readonly: !!s.ro, status: s.st === "run" ? "running" : s.st === "ended" ? "ended" : "idle",
      used: s.ctxUsed, size: s.ctxSize,
      created: s.since ?? 0, updated: s.at ?? 0, busy: !!s.busy, queued: s.queued ?? s.queue?.length ?? 0,
      waiting: s.room ?? undefined,
    }) satisfies SessionView,
  })))
}

export function seedSessionViews(views: SessionView[]) {
  patch(views.map((view): PatchOp => ({ type: "session", op: "upsert", id: view.id, view })))
}

// These fixtures render components directly and seed all their rows above.
api.itemSessions = async () => ({ rev, views: [], missing: [] })

let seededMessages = new Set<string>()
export function seedMessages(messages: (Msg | ConversationMessage)[], replace = true) {
  const ops: PatchOp[] = []
  const next = new Set<string>()
  for (const input of messages) {
    if (!input.id) continue
    const view: MessageView = "author" in input ? { ...input, ws: "default" } : messageView({
      id: input.id, author: input.k, text: input.t ?? "",
      item_id: input.item ?? null, created: input.created ?? 0,
      meta: { ...input, reply_to: input.replyTo, card_only: input.cardOnly, was_card: input.wasCard, a2ui_action: input.a2uiAction },
    })
    ops.push({ type: "message", op: "upsert", id: view.id, view })
    next.add(view.id)
  }
  if (replace) {
    for (const id of seededMessages) if (!next.has(id)) ops.push({ type: "message", op: "remove", id })
    seededMessages = next
  }
  patch(ops)
}
