/* Synthetic Hub responses for the real app, not a replacement ChatView/SidePanel. */
import type { RBot, RItemDetail, RWorkspace, Shell } from "../../src/api.ts"
import type { ItemView, MessageView, Snapshot } from "../../src/gen/index.ts"
import { itemView, messageView, pageResponse, snapshotViews, viewsResponse } from "./wire-views.ts"

export type ChatPanelFixtureState = {
  shell: Shell
  snapshot: Snapshot
  items: ItemView[]
  messages: MessageView[]
}

export function chatPanelState(large = false): ChatPanelFixtureState {
  const created = Date.UTC(2026, 0, 1)
  const workspace: RWorkspace = { id: "panel-fixture", name: "Panel fixture", description: "", item_prefix: "ST", sort: 0, created }
  const bot: RBot = {
    id: "steve", name: "Steve", role: "Assistant", main: true, harness: "fixture", model: null,
    rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "ask", avatar: {},
    default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: created,
    workspaces: [workspace.id],
  }
  const items: ItemView[] = Array.from({ length: large ? 400 : 12 }, (_, n) => itemView({
    id: `ST-${n + 1}`, ws: workspace.id, no: n + 1, title: `Synthetic task ${n + 1}`,
    state: n % 3 ? { kind: "active" } : { kind: "paused" },
    bot_id: bot.id, source: "chat", created, updated: created, goals: 1,
  }))
  const messages: MessageView[] = Array.from({ length: large ? 200 : 24 }, (_, n) => messageView({
    id: `panel-${n}`, ws: workspace.id, author: n % 2 ? "steve" : "me",
    text: `Synthetic conversation ${n}. A message with enough words to wrap when the docked panel opens. `.repeat(n % 5 === 0 ? 8 : 1),
    created: created + n * 60_000 + Math.floor(n / 8) * 7_200_000,
  }))
  const shell: Shell = {
    modules: { feishuRegistration: false }, version: "fixture", web_version: "fixture",
    workspace, workspaces: [workspace],
    bots: [bot], allBots: [{ id: bot.id, name: bot.name, workspaces: bot.workspaces }],
    projects: [], tags: [], settings: { onboard: "1" }, notifySeen: created,
  }
  const snapshot = snapshotViews({
    item: items, session: [], decision: [],
    message: pageResponse(messages, 1, null),
  })
  return { shell, snapshot, items, messages }
}

type RouteLike = {
  request: () => { url: () => string }
  continue: () => Promise<void>
  fulfill: (opts: { status?: number; json?: unknown }) => Promise<void>
}
type PageLike = {
  route: (pattern: string, handler: (route: RouteLike) => Promise<void> | void) => Promise<void>
}

export async function routeChatPanelHub(page: PageLike, state: ChatPanelFixtureState) {
  const calls: string[] = []
  await page.route("**/*", (route) => {
    const u = new URL(route.request().url()), p = u.pathname
    if (p === "/auth/hello") return route.fulfill({ json: { name: "steve", version: "fixture", api: 2, login: { token: false, oidc: false } } })
    if (!p.startsWith("/api/")) return route.continue()
    calls.push(p)
    if (p === "/api/shell") return route.fulfill({ json: state.shell })
    if (p === "/api/summary") return route.fulfill({ json: { ws: state.shell.workspace.id, workspace_name: state.shell.workspace.name, thinking: false, hold: null } })
    if (p === "/api/snapshot") return route.fulfill({ json: state.snapshot })
    if (p === "/api/views") return route.fulfill({ json: viewsResponse([], 1) })
    if (p === "/api/notifications") return route.fulfill({ json: [] })
    if (p === "/api/me") return route.fulfill({ json: { user: { id: "u1", name: "Fixture", email: null, role: "owner" }, version: "fixture", web_version: "fixture" } })
    if (p === "/api/conversations/channels") return route.fulfill({ json: { channels: [] } })
    // Keep the event-stream request pending until the page closes. A fulfilled finite SSE body
    // would reconnect and rebuild /api/shell periodically, contaminating the panel-only timings.
    if (p === "/api/stream") return
    if (p === "/api/plugins/themes") return route.fulfill({ json: { themes: [] } })
    if (p === "/api/thread/search") return route.fulfill({ json: { messages: state.messages.filter((m) => m.text.includes(u.searchParams.get("q") || "")).reverse() } })
    if (p.startsWith("/api/items/")) {
      const item = state.items.find((i) => p === `/api/items/${i.id}`)
      if (item) {
        const detail: RItemDetail = {
          id: item.id, source: ["chat", "chat"], plan: [],
          goalsList: [{ version: 1, text: "Fixture", created: item.created }],
          timeline: [], decisions: [], dependents: [], annotations: [], artifacts: [],
        }
        return route.fulfill({ json: detail })
      }
    }
    if (p === "/api/conversations") return route.fulfill({ json: { conversations: [], has_more: false, next_offset: null, total: 0 } })
    return route.fulfill({ status: 404, json: {} })
  })
  return calls
}
