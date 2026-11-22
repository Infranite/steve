/* Synthetic Hub responses for the real app, not a replacement ChatView/SidePanel. */
export function chatPanelState(large = false) {
  const created = Date.UTC(2026, 0, 1), workspace = { id: "panel-fixture", name: "Panel fixture", item_prefix: "ST", created, updated: created }
  const bot = { id: "steve", name: "Steve", role: "Assistant", main: true, harness: "fixture", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "default", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: created, workspaces: [workspace.id], memories: 0 }
  const items = Array.from({ length: large ? 400 : 12 }, (_, n) => ({ id: `ST-${n + 1}`, workspace_id: workspace.id, no: n + 1, title: `Synthetic task ${n + 1}`, status: n % 3 ? "run" : "pause", stage: "", bot_id: bot.id, project_id: null, cwd: null, branch: null, plan: [], now: "", accept_mode: null, priority: "", archived: false, star: false, due: null, snooze: null, source: ["chat", "chat"], parent: null, created, updated: created, goal: "Fixture", goals: 1, tags: [], deps: [], sessionIds: [], open: [], queued: 0, live: false }))
  const thread = Array.from({ length: large ? 200 : 24 }, (_, n) => ({ id: `panel-${n}`, author: n % 2 ? "steve" : "me", text: `Synthetic conversation ${n}. A message with enough words to wrap when the docked panel opens. ` .repeat(n % 5 === 0 ? 8 : 1), item_id: null, meta: null, created: created + n * 60_000 + Math.floor(n / 8) * 7_200_000 }))
  return { version: "fixture", web_version: "fixture", workspace, workspaces: [workspace], items, decisions: [], recent: [], bots: [bot], allBots: [bot], projects: [], tags: [], tagUses: {}, thread, settings: { onboard: "1" }, sessions: [], devices: [], runtimes: [], notices: [] }
}

export async function routeChatPanelHub(page, state) {
  const calls = []
  await page.route("**/*", route => {
    const u = new URL(route.request().url()), p = u.pathname
    if (p === "/auth/hello") return route.fulfill({ json: { name: "steve", version: "fixture", api: 1, login: { token: false, oidc: false } } })
    if (!p.startsWith("/api/")) return route.continue()
    calls.push(p)
    if (p === "/api/shell") return route.fulfill({ json: state })
    // Keep the event-stream request pending until the page closes. A fulfilled finite SSE body
    // would reconnect and rebuild /api/state periodically, contaminating the panel-only timings.
    if (p === "/api/stream") return
    if (p === "/api/plugins/themes") return route.fulfill({ json: { themes: [] } })
    if (p === "/api/thread/search") return route.fulfill({ json: { messages: state.thread.filter(m => m.text.includes(u.searchParams.get("q") || "")).reverse() } })
    if (p.startsWith("/api/items/")) {
      const item = state.items.find(i => p === `/api/items/${i.id}`)
      if (item) return route.fulfill({ json: { ...item, goalsList: [], timeline: [], decisions: [], dependents: [], annotations: [], artifacts: [] } })
    }
    if (p === "/api/conversations") return route.fulfill({ json: { conversations: [], has_more: false, next_offset: null, total: 0 } })
    if (p === "/api/yolo" || p === "/api/yolo/actions") return route.fulfill({ json: { windows: [], on: false, mode: "off", scopes: [], counts: {} } })
    return route.fulfill({ status: 404, json: {} })
  })
  return calls
}
