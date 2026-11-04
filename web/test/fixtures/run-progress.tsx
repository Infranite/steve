import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import { api, type REvent, type RItemDetail, type RSession, type State } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useStore } from "../../src/store"
import { ItemView } from "../../src/views"

/* ST-48: one session; its first turn delivered, the user approved in a comment, and the turn after the approval runs
 * (`?done`: it finished; `?perm`: it stopped for a tool permission midway, then went on). The approved turn is the
 * comment's reply; the session block must not show it a second time, and both must time it from its prompt. */
const q = new URLSearchParams(location.search), done = q.has("done"), perm = q.has("perm")
document.documentElement.classList.toggle("dark", q.has("dark"))
/* minutes before the page loaded, so live clocks read the same after a reload */
const T = Number(q.get("t") ?? Date.now()), m = (n: number) => T - (30 - n) * 60_000
const status: RSession["status"] = done ? "ended" : "running"
const sessions: RSession[] = [{ id: "s1", workspace_id: "default", item_id: "ST-7", no: 1, bot_id: "dev", harness: "claude-code", model: "claude-opus-5-5", cwd: "/tmp/st-7", status, used: 36000, size: 200000, cost: null, title: "推送分支并发布", device_id: "hub", source: "item", readonly: false, busy: !done, queued: [], created: m(1), updated: done ? m(26) : m(20) } as RSession]
const line = (id: number, n: number, kind: string, text: string, session_id: string | null = null, actor = "steve") => ({ id, ts: m(n), actor, kind, text, session_id })
const timeline = [
  line(1, 0, "sys", "Steve 建立事项，交给 Steve 开发 Bot"),
  line(2, 1, "session", "开始会话 #1", "s1"),
  line(3, 2, "sys", "更新了计划"),
  line(4, 9, "sys", "交付，等你验收", "s1"),
  line(5, 12, "me", "批准：先腾出磁盘空间，再推送 steve/st-41", "s1", "me"),
]
let seq = 0
const ev = (n: number, kind: string, data: any): REvent => ({ seq: ++seq, ts: m(n), kind, data })
const run = (n: number, cmd: string, open = false): REvent => ev(n, "tool_call", { toolCallId: "t" + seq, title: cmd, kind: "execute", status: open ? "in_progress" : "completed", rawInput: { command: cmd } })
const edit = (n: number, file: string): REvent => ev(n, "tool_call", { toolCallId: "t" + seq, title: "Edit " + file, kind: "edit", status: "completed", locations: [{ path: file }], rawInput: { file_path: file, old_string: "a", new_string: "b" } })
const events: REvent[] = [
  ev(1, "user", { text: "推送 steve/st-41 并发布", via: "steve" }), run(2, "git status"), edit(3, "/tmp/st-7/README.md"),
  ev(8, "agent_message_chunk", { content: { text: "等你批准后推送。" } }), ev(9, "turn_end", { stopReason: "end_turn" }),
  ev(12, "user", { text: "用户批准了：先腾出磁盘空间，再推送 steve/st-41", via: "steve" }),
  run(13, "df -h ."), run(13, "rm -rf node_modules/.cache"), edit(14, "/tmp/st-7/ops/push.sh"),
  ...(perm ? [ev(15, "permission", { title: "git push origin steve/st-41", decision: "perm-1" }), run(18, "git push origin steve/st-41")] : [run(15, "git push origin steve/st-41")]),
  edit(16, "/tmp/st-7/ops/push.sh"), run(17, "gh pr create", !done),
  ...(done ? [ev(25, "agent_message_chunk", { content: { text: "已推送并开了 PR。" } }), ev(26, "turn_end", { stopReason: "end_turn" })] : []),
]
const item = { id: "ST-7", workspace_id: "default", no: 7, title: "推送分支并发布", status: done ? "wait" : "run", stage: "", bot_id: "dev", project_id: null, cwd: null, branch: null, plan: [{ step: "推送", done: false }], now: "正在推送", accept_mode: null, priority: "中", archived: false, star: false, due: null, snooze: null, source: ["对话", "thread"], parent: null, created: m(0), updated: m(20), goal: "推送并发布", goals: 1, tags: [], deps: [], sessionIds: ["s1"], open: [], queued: 0, live: !done } as unknown as RItemDetail
const detail: RItemDetail = { ...item, goalsList: [{ version: 1, text: item.goal, created: m(0) }], timeline, artifacts: [], decisions: [], dependents: [] }
const state = { version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items: [item], decisions: [], recent: [], allBots: [],
  bots: [{ id: "dev", name: "Steve 开发 Bot", role: "", main: false, harness: "claude-code", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions, devices: [{ id: "hub", name: "Hub", roles: [], os: "", version: "", status: "online", max_sessions: 4, seen: 0, created: 0 }], runtimes: [] } as unknown as State
api.state = async () => structuredClone(state)
api.item = async () => structuredClone(detail)
api.events = async (_id: string, after = 0) => events.filter((e) => e.seq > after)
void refresh()
function Fixture() { useStore(); return <TooltipProvider><SidebarProvider><main className="w-full"><ItemView id="ST-7" /></main><Toaster /></SidebarProvider></TooltipProvider> }
createRoot(document.getElementById("root")!).render(<Fixture />)
