import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import { api, type REvent, type RItemDetail, type RSession, type State } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useStore } from "../../src/store"
import { ItemView } from "../../src/views"

/* ST-42: an item page fed through the store as the Hub sends it. Session #1 ended, then a new request started session #2,
 * which runs; a comment to #2 is being answered. Plan changes are written while the sessions run. */
document.documentElement.classList.toggle("dark", location.search.includes("dark"))
const m = (n: number) => Date.UTC(2026, 10, 5, 0, n)
const sess = (id: string, no: number, status: RSession["status"], created: number, updated: number): RSession => ({ id, workspace_id: "default", item_id: "ST-9", no, bot_id: "dev", harness: "claude-code", model: "claude-opus-5-5", cwd: "/tmp/st-9", status, used: 36000, size: 200000, cost: null, title: "会话中间过程事件排序修正", device_id: "hub", source: "item", readonly: false, busy: status === "running", queued: [], created, updated })
const sessions = [sess("s1", 1, "ended", m(8), m(20)), sess("s2", 2, "running", m(30), m(41))]
const line = (id: number, n: number, kind: string, text: string, session_id: string | null = null, actor = "steve") => ({ id, ts: m(n), actor, kind, text, session_id })
const timeline = [
  line(1, 7, "sys", "Steve 建立事项，交给 Steve 开发 Bot"),
  line(2, 8, "session", "开始会话 #1", "s1"),
  line(3, 9, "sys", "更新了计划"),
  line(4, 14, "sys", "换了一版计划，上一版没做完的 6 步已废弃"),
  line(5, 19, "sys", "交付，等你验收", "s1"),
  line(6, 22, "sys", "验收没通过，会再开会话"),
  line(7, 29, "me", "再加一个需求：深色模式下也要检查", null, "me"),
  line(8, 30, "session", "开始会话 #2", "s2"),
  line(9, 31, "sys", "更新了计划"),
  line(10, 33, "sys", "换了一版计划，上一版没做完的 2 步已废弃"),
  line(11, 39, "me", "顺便看一下手机端", "s2", "me"),
  line(12, 41, "sys", "更新了计划"),
]
let seq = 0
const ev = (n: number, kind: string, data: any): REvent => ({ seq: ++seq, ts: m(n), kind, data })
const tool = (n: number, title: string): REvent => ev(n, "tool_call", { toolCallId: "t" + seq, title, kind: "execute", status: "completed" })
const events: Record<string, REvent[]> = {
  s1: [ev(8, "user", { text: "修复过程事件的显示顺序", via: "steve" }), tool(9, "运行 npm run check"), ev(19, "agent_message_chunk", { content: { text: "做完了，等你验收。" } }), ev(19, "turn_end", { stopReason: "end_turn" })],
  s2: [ev(30, "user", { text: "补充信息：再加一个需求：深色模式下也要检查", via: "steve" }), tool(31, "运行 npm run check"), ev(33, "agent_message_chunk", { content: { text: "深色模式查过了。" } }), ev(33, "turn_end", { stopReason: "end_turn" }),
    ev(39, "user", { text: "补充信息：顺便看一下手机端", via: "me" }), tool(40, "运行 npm run test:web"), tool(41, "运行 sed -i '' 's/a/b/' src/views.tsx")],
}
const item = { id: "ST-9", workspace_id: "default", no: 9, title: "会话中间过程事件排序修正", status: "run", stage: "", bot_id: "dev", project_id: null, cwd: null, branch: null, plan: [{ step: "修正排序", done: false }], now: "正在改排序", accept_mode: null, priority: "中", archived: false, star: false, due: null, snooze: null, source: ["对话", "thread"], parent: null, created: m(7), updated: m(41), goal: "会话的动态部分始终在最下方", goals: 1, tags: [], deps: [], sessionIds: ["s1", "s2"], open: [], queued: 0, live: true } as unknown as RItemDetail
const detail: RItemDetail = { ...item, goalsList: [{ version: 1, text: item.goal, created: m(7) }], timeline, artifacts: [], decisions: [], dependents: [] }
const state = { version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items: [item], decisions: [], recent: [], allBots: [],
  bots: [{ id: "dev", name: "Steve 开发 Bot", role: "", main: false, harness: "claude-code", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions, devices: [{ id: "hub", name: "Hub", roles: [], os: "", version: "", status: "online", max_sessions: 4, seen: 0, created: 0 }], runtimes: [] } as unknown as State
api.state = async () => structuredClone(state)
api.item = async () => structuredClone(detail)
api.events = async (id: string, after = 0) => (events[id] ?? []).filter((e) => e.seq > after)
void refresh()
function Fixture() { useStore(); return <TooltipProvider><SidebarProvider><main className="w-full"><ItemView id="ST-9" /></main><Toaster /></SidebarProvider></TooltipProvider> }
createRoot(document.getElementById("root")!).render(<Fixture />)
