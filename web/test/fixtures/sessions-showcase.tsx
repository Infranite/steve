import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import { api, type RDecision, type RItem, type RSession, type State } from "../../src/api"
import { SidebarProvider, SidebarInset } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { setLang } from "../../src/i18n"
import { refresh, useStore } from "../../src/store"
import { SessionsView } from "../../src/sessions-list"

/* the 会话 list with a realistic spread: live, waiting, ended, imported and unlinked conversations across a week, on
 * several models and two bots. ?dark · ?en · ?narrow is left to the viewport */
const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark"))
if (q.has("en")) void setLang("en")
const now = Date.now(), H = 3_600_000
const titles = ["重设计多 Bot 协作区的信息展示", "修复完全放开后工具仍反复请求审批", "独立审查权限切换及隔离安全", "修复协作工作流状态检测与异常卡误报", "独立审查状态判定及回归覆盖", "统一工作状态判定与异常恢复", "整合协作区与关系图待确认设计", "统一执行体完整边界并修复关闭权限状态", "独立审查协作区与关系图设计", "修正执行证据、暂停结果与周期身份", "修订协作区与关系图统一设计", "补齐会话页的搜索高亮", "迁移旧版通知到信号中心", "Apple 客户端的同步策略", "整理事项列表的工具栏"]
const models = ["gpt-6.1-sol", "gpt-6.1-sol", "gpt-6.1-sol", "claude-opus-5-5", "gemini-4-carbon-high"]
const items: RItem[] = []
const sessions: RSession[] = []
for (let n = 0; n < 46; n++) {
  const no = 57 - n, id = "ST-" + no, title = titles[n % titles.length]
  const unlinked = n % 13 === 9, imported = n === 20 || n === 31
  if (!unlinked && !imported) items.push({ id, workspace_id: "default", no, title: n % 5 === 4 ? "统一工作状态判定（第 " + (n % 3 + 1) + " 轮）" : title, status: n < 3 ? "run" : "done", stage: "", bot_id: n % 4 === 2 ? "rev" : "dev", project_id: "p1", cwd: null, branch: null, plan: [], now: "", accept_mode: null, priority: "中", archived: false, star: false, due: null, snooze: null, source: ["对话", "chat"], parent: null, created: now - n * 3 * H, updated: now - n * 3 * H, goal: title, goals: 1, tags: [], deps: [], sessionIds: [], open: [], queued: 0, live: n < 3 } as unknown as RItem)
  const status: RSession["status"] = n === 0 || n === 2 ? "running" : n % 3 === 0 ? "ended" : "idle"
  sessions.push({ id: "s" + n, workspace_id: "default", item_id: unlinked || imported ? null : id, no: n % 7 === 5 ? 2 : 1, bot_id: imported ? null : n % 4 === 2 ? "rev" : "dev", harness: imported ? "codex" : "claude-code", model: models[n % models.length], cwd: "/srv/code/steve", status,
    used: n % 6 === 1 ? null : [54, 77, 70, 81, 12, 28, 70, 60, 71, 67, 30, 34, 78, 74, 91][n % 15] * 2000, size: 200000, cost: null, title: imported ? "本机 Codex：排查构建缓存" : title, device_id: "hub", source: imported ? "本机 Codex" : "Steve", readonly: imported, busy: status === "running", queued: [],
    created: now - (n * 3.1 + 1) * H, updated: now - n * 3.1 * H - 7 * 60_000 } as RSession)
}
const decisions: RDecision[] = [{ id: "d1", kind: "approve", subject_type: "tool_call", subject_id: "{}", item_id: "ST-56", session_id: "s1", question: "允许执行：npm run build", draft: "", options: ["允许一次", "拒绝"], status: "open", result: null, requested_by: "dev", created: now - H, resolved: null, undo: null } as unknown as RDecision]
const bot = (id: string, name: string, color: string) => ({ id, name, role: "", main: false, harness: "claude-code", model: "gpt-6.1-sol", rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: { color }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 })
const state = { version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items, decisions, recent: [], allBots: [],
  bots: [bot("dev", "开发 Bot", "#e5484d"), bot("rev", "复核 Bot", "#3e63dd")],
  projects: [{ id: "p1", workspace_id: "default", name: "Steve", path: "/srv/code/steve", description: "", kind: "git", worktree: true, default_bot: "dev", release: null, archived: false, device_id: "hub", created: 0, updated: 0 }],
  tags: [], tagUses: {}, thread: [], settings: {}, sessions,
  devices: [{ id: "hub", name: "PENGXIANG's Mac Mini", roles: ["Hub"], os: "", version: "", status: "online", max_sessions: 4, seen: 0, created: 0 }], runtimes: [] } as unknown as State
api.state = async () => structuredClone(state)
api.events = async () => []
api.searchSessions = async (s: string) => (s === "缓存" ? ["s20", "s4"] : [])
void refresh()
function Fixture() {
  useStore()
  return <TooltipProvider><SidebarProvider><SidebarInset className="relative h-dvh min-w-0 overflow-hidden"><SessionsView /></SidebarInset><Toaster /></SidebarProvider></TooltipProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
