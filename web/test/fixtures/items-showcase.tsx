import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { stats } from "../../src/api/stats"
import { api, type RDecision, type RItem, type RItemDetail, type RSession, type State } from "../../src/api"
import { SidebarProvider, SidebarInset } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { setLang } from "../../src/i18n"
import { refresh, useStore } from "../../src/store"
import { InboxView, ItemsView, ItemView } from "../../src/views"

/* the items module in states the dev Hub rarely has at once: an item waiting on two decisions with plan rounds,
 * dependencies, tags and two children; an inbox with decisions; every status in the list and board.
 * ?view=item|inbox|items · ?dark · ?en */
const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark"))
if (q.has("en")) void setLang("en")
const m = (n: number) => Date.UTC(2026, 10, 12, 0, n)
const sess = (id: string, no: number, status: RSession["status"], item: string): RSession => ({ id, workspace_id: "default", item_id: item, no, bot_id: "dev", harness: "claude-code", model: "claude-opus-5-5", cwd: "/tmp/" + item, status, used: 64000, size: 200000, cost: null, title: "", device_id: "hub", source: "item", readonly: false, busy: status === "running", queued: [], created: m(5), updated: m(40) } as RSession)
const base = (id: string, title: string, status: RItem["status"], extra: Partial<RItem> = {}): RItem => ({
  id, workspace_id: "default", no: +id.split("-")[1], title, status, stage: "", bot_id: "dev", project_id: "p1", cwd: null, branch: null, plan: [], now: "", accept_mode: null, priority: "中",
  archived: false, star: false, due: null, snooze: null, source: ["对话", "chat"], parent: null, created: m(1), updated: m(40), goal: title, goals: 1, tags: [], deps: [], sessionIds: [], open: [], queued: 0, live: false, ...extra,
} as RItem)
const dec = (id: string, item: string, kind: RDecision["kind"], question: string, options: string[], draft = ""): RDecision => ({ id, kind, subject_type: kind === "accept" ? "delivery" : "question", subject_id: "{}", item_id: item, session_id: null, question, draft, options, status: "open", result: null, requested_by: "dev", created: m(38), resolved: null, undo: null })
const decisions = [
  dec("d1", "ST-9", "ask", "设置页的分组标题用中文还是英文？", ["中文", "英文", "跟随界面语言"], "我倾向跟随界面语言：标题和其余文案一起切换。"),
  dec("d2", "ST-9", "approve", "允许执行：npm run build:web", ["允许一次", "拒绝"]),
  dec("d3", "ST-5", "accept", "设计稿已交付，请验收", ["通过", "返工"], "三个页面的深浅色截图都在产出里。"),
]
const items: RItem[] = [
  base("ST-9", "重做事项页的视觉层级与动效", "wait", { plan: [{ step: "梳理页面结构与信息层级", done: true }, { step: "替换字号与圆角到设计令牌", done: true }, { step: "卡片改为分层表面，去掉嵌套边框", done: false }, { step: "补展开收起与列表进入动效", done: false }, { step: "深浅色与手机宽度截图核对", done: false }],
    now: "正在把卡片改为分层表面", priority: "高", due: "明天", tags: ["t1", "t2"], deps: ["ST-3", "ST-4"], sessionIds: ["s1", "s2"], open: ["d1", "d2"], goals: 2, star: true, cwd: "/srv/code/steve", branch: "ds/items",
    goal: "让事项页读起来像一份清楚的工作记录：**标题、目标、此刻的状态、对话**依次排开；右侧是进展与属性。\n\n- 只用设计系统里的字号和圆角\n- 状态色只表示状态\n- 展开收起有高度动画\n- 卡片用分层表面，不用边框套边框\n- 列表进入有淡入\n- 手机宽度下看板一列一屏\n- 深浅色都核对过截图" }),
  base("ST-12", "整理事项列表的工具栏", "run", { parent: { item: "ST-9", at: 3 }, now: "在对齐筛选控件的高度", plan: [{ step: "对齐", done: true }, { step: "截图", done: false }], sessionIds: ["s3"], live: true }),
  base("ST-13", "看板列的空状态", "done", { parent: { item: "ST-9", at: 4 }, now: "已交付" }),
  base("ST-3", "设计令牌落地到 index.css", "done", { now: "已合入 design/system-v2" }),
  base("ST-4", "基础组件换新", "run", { now: "Tabs 的滑动指示器", live: true, sessionIds: ["s4"], plan: [{ step: "Button", done: true }, { step: "Tabs", done: false }] }),
  base("ST-5", "三个原型页面的设计稿", "wait", { open: ["d3"], now: "等你验收", tags: ["t3"] }),
  base("ST-6", "Apple 客户端的同步策略", "pause", { now: "等上游接口定稿", project_id: null }),
  base("ST-7", "统计页图表重画", "fail", { now: "构建失败：类型检查没过" }),
  base("ST-8", "旧版侧栏的兼容", "drop", { now: "不再需要" }),
  base("ST-2", "字体换成 Geist", "done", { now: "已交付，深浅色都核对过" }),
]
const timeline = [
  { id: 1, ts: m(1), actor: "steve", kind: "sys", text: "Steve 建立事项，交给开发 Bot", session_id: null },
  { id: 2, ts: m(2), actor: "steve", kind: "session", text: "开始会话 #1", session_id: "s1" },
  { id: 3, ts: m(3), actor: "steve", kind: "sys", text: "拆出子事项 ST-12", session_id: null },
  { id: 4, ts: m(4), actor: "steve", kind: "sys", text: "拆出子事项 ST-13", session_id: null },
  { id: 5, ts: m(5), actor: "steve", kind: "sys", text: "更新了计划", session_id: null },
  { id: 6, ts: m(20), actor: "me", kind: "me", text: "深色模式下状态色别发灰，用 /10 的底色", session_id: null },
  { id: 7, ts: m(30), actor: "steve", kind: "session", text: "开始会话 #2", session_id: "s2" },
  { id: 8, ts: m(38), actor: "steve", kind: "sys", text: "有两件事需要你定", session_id: null },
]
const detail = (i: RItem): RItemDetail => ({ ...i, goalsList: [{ version: 1, text: "让事项页更精致", created: m(1) }, { version: 2, text: i.goal, created: m(10) }],
  planRounds: i.id === "ST-9" ? [{ round: 1, steps: [{ step: "直接改样式", done: true }, { step: "补测试", done: false }], end: "replaced", created: m(6) }] : [],
  timeline: i.id === "ST-9" ? timeline : [], artifacts: [], decisions: decisions.filter((d) => d.item_id === i.id), dependents: [] })
const state = { version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items, decisions, recent: [], allBots: [],
  bots: [{ id: "dev", name: "开发 Bot", role: "", main: false, harness: "claude-code", model: "claude-opus-5-5", rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 },
    { id: "rev", name: "复核 Bot", role: "", main: false, harness: "codex", model: "gpt-5.5", rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: { color: "#3e63dd" }, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [{ id: "p1", workspace_id: "default", name: "Steve", path: "/srv/code/steve", description: "", kind: "git", worktree: true, default_bot: "dev", release: null, archived: false, device_id: "hub", created: 0, updated: 0 }],
  tags: [{ id: "t1", name: "设计", cat: "领域", desc: "视觉与交互" }, { id: "t2", name: "Web", cat: "领域", desc: "web 客户端" }, { id: "t3", name: "原型", cat: "领域", desc: "" }], tagUses: {}, thread: [], settings: {},
  sessions: [sess("s1", 1, "ended", "ST-9"), sess("s2", 2, "running", "ST-9"), sess("s3", 1, "running", "ST-12"), sess("s4", 1, "running", "ST-4")],
  devices: [{ id: "hub", name: "Hub", roles: ["Hub"], os: "", version: "", status: "online", max_sessions: 4, seen: 0, created: 0 }], runtimes: [] } as unknown as State
api.state = async () => structuredClone(state)
api.item = async (id: string) => structuredClone(detail(items.find((i) => i.id === id) ?? items[0]))
api.events = async () => []
/* no Hub behind the fixture: the item's figures are simply absent (a 401 would send the page to sign in) */
stats.item = async () => { throw new Error("no stats in the fixture") }
stats.tree = async () => { throw new Error("no stats in the fixture") }
void refresh()
const view = q.get("view") ?? "item"
function Fixture() {
  useStore()
  return <TooltipProvider><SidebarProvider><SidebarInset className="relative h-dvh min-w-0 overflow-hidden">
    {view === "inbox" ? <InboxView /> : view === "items" ? <ItemsView /> : <ItemView id="ST-9" />}
  </SidebarInset><Toaster /></SidebarProvider></TooltipProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
