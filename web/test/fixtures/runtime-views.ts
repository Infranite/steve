/* Typed wire views for runtime-scope.browser.mjs and runtime-setup.browser.mjs. */
import type { RItemDetail } from "../../src/api.ts"
import type { ById, ItemView, SessionView, Snapshot } from "../../src/gen/index.ts"
import { itemView, sessionView, snapshotViews, viewsResponse } from "./wire-views.ts"

export function runtimeScopeItemView(input: {
  id: string
  ws: string
  no: number
  title: string
  mode: "start" | "propose"
  bot_id: string
  project_id: string | null
  priority: string
  created: number
  updated: number
}): ItemView {
  return itemView({
    id: input.id,
    ws: input.ws,
    no: input.no,
    title: input.title,
    state: input.mode === "propose" ? { kind: "ready" } : { kind: "active" },
    bot_id: input.bot_id,
    project_id: input.project_id ?? undefined,
    priority: input.priority,
    source: "web",
    created: input.created,
    updated: input.updated,
    goals: 1,
  })
}

export function runtimeSetupItemView(goal: string, botId: string): ItemView {
  return itemView({
    id: "ST-900",
    ws: "fixture",
    no: 900,
    title: goal,
    state: { kind: "active" },
    revision: 0,
    bot_id: botId,
    source: "web",
    goals: 1,
  })
}

export function runtimeSetupItemDetail(goal: string): RItemDetail {
  return {
    id: "ST-900",
    source: ["web", ""],
    cwd: "/fixture",
    branch: null,
    plan: [],
    goalsList: [{ version: 1, text: goal, created: 0 }],
    timeline: [],
    artifacts: [],
    decisions: [],
    dependents: [],
  }
}

export function runtimeSetupSessionView(
  itemId: string,
  bot: { id: string; harness: string; model: string | null },
  title: string,
): SessionView {
  return sessionView({
    id: "fixture-session",
    ws: "fixture",
    item_id: itemId,
    no: 1,
    bot_id: bot.id,
    harness: bot.harness,
    model: bot.model ?? undefined,
    cwd: "/fixture",
    status: "starting",
    title,
    device_id: "hub",
    source: "Steve",
  })
}

export function runtimeSnapshot(items: ItemView[], sessions: SessionView[], epoch = "fixture"): Snapshot {
  return snapshotViews({ item: items, session: sessions, decision: [] }, { epoch, rev: 1 })
}

export function runtimeViews(views: (ItemView | SessionView)[], rev = 2): ById {
  return viewsResponse(views, rev)
}
