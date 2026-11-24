/* Synthetic Hub responses for the real app, not a replacement ChatView/SidePanel. */
export function chatPanelState(large = false) {
  const created = Date.UTC(2026, 0, 1), workspace = { id: "panel-fixture", name: "Panel fixture", description: "", item_prefix: "ST", sort: 0, created, updated: created }
  const bot = { id: "steve", name: "Steve", role: "Assistant", main: true, harness: "fixture", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "default", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: created, workspaces: [workspace.id], memories: 0 }
  const itemViews = Array.from({ length: large ? 400 : 12 }, (_, n) => ({
    id: `ST-${n + 1}`, ws: workspace.id, no: n + 1, title: `Synthetic task ${n + 1}`,
    state: n % 3 ? { kind: "active" } : { kind: "paused" }, cycle: 1, revision: 1, delivery_revision: 0, delivery: [], waits: [],
    bot_id: bot.id, priority: "中", star: false, archived: false, design_gate: false, source: "chat",
    created, updated: created, plan_done: 0, plan_total: 0, goals: 1, tags: [], deps: [], op_blockers: [],
    detail_stamp: 0, active_operations: 0, has_branch: false,
  }))
  const thread = Array.from({ length: large ? 200 : 24 }, (_, n) => ({ id: `panel-${n}`, ws: workspace.id, author: n % 2 ? "steve" : "me", text: `Synthetic conversation ${n}. A message with enough words to wrap when the docked panel opens. ` .repeat(n % 5 === 0 ? 8 : 1), item_id: null, meta: null, created: created + n * 60_000 + Math.floor(n / 8) * 7_200_000 }))
  return { version: "fixture", web_version: "fixture", owner: false, workspace, workspaces: [workspace], bots: [bot], allBots: [{ id: bot.id, name: bot.name, workspaces: bot.workspaces }], projects: [], tags: [], settings: { onboard: "1" }, notifySeen: created, itemViews, thread }
}

export async function routeChatPanelHub(page, state) {
  const calls = []
  await page.route("**/*", route => {
    const u = new URL(route.request().url()), p = u.pathname
    if (p === "/auth/hello") return route.fulfill({ json: { name: "steve", version: "fixture", api: 2, login: { token: false, oidc: false } } })
    if (!p.startsWith("/api/")) return route.continue()
    calls.push(p)
    if (p === "/api/shell") { const { itemViews: _i, thread: _t, ...shell } = state; return route.fulfill({ json: shell }) }
    if (p === "/api/summary") return route.fulfill({ json: { ws: state.workspace.id, thinking: false, hold: null } })
    if (p === "/api/snapshot") return route.fulfill({ json: { epoch: "fixture", rev: 1, cursor: "fixture:1", views: { item: state.itemViews, session: [], decision: [], message: { views: state.thread, next: null } } } })
    if (p === "/api/views") return route.fulfill({ json: { rev: 1, views: [], missing: [] } })
    if (p === "/api/notifications") return route.fulfill({ json: [] })
    if (p === "/api/me") return route.fulfill({ json: { user: { id: "u1", name: "Fixture", email: null, role: "owner" }, version: "fixture", web_version: "fixture" } })
    if (p === "/api/conversations/channels") return route.fulfill({ json: { channels: [] } })
    // Keep the event-stream request pending until the page closes. A fulfilled finite SSE body
    // would reconnect and rebuild /api/shell periodically, contaminating the panel-only timings.
    if (p === "/api/stream") return
    if (p === "/api/plugins/themes") return route.fulfill({ json: { themes: [] } })
    if (p === "/api/thread/search") return route.fulfill({ json: { messages: state.thread.filter(m => m.text.includes(u.searchParams.get("q") || "")).reverse() } })
    if (p.startsWith("/api/items/")) {
      const item = state.itemViews.find(i => p === `/api/items/${i.id}`)
      if (item) return route.fulfill({ json: { ...item, workspace_id: state.workspace.id, status: item.state.kind === "paused" ? "pause" : "run", stage: "", goal: "Fixture", goalsList: [], timeline: [], decisions: [], dependents: [], annotations: [], artifacts: [] } })
    }
    if (p === "/api/conversations") return route.fulfill({ json: { conversations: [], has_more: false, next_offset: null, total: 0 } })
    return route.fulfill({ status: 404, json: {} })
  })
  return calls
}
