import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SidebarInset, SidebarProvider } from "../../src/components/ui/sidebar"
import { st, useStore, type Item, type Bot, type Wait } from "../../src/store"
import { Page } from "../../src/ui"
import { Sidebar } from "../../src/shell"
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
/* the running-status pill in the sidebar and, on a phone, in the page header: an item that works, one that asks, one
 * paused (it resumes from the pill), one whose graph is paused (it resumes on its page), one done (not in the pill).
 * ?none: nothing open, so a phone's header has no pill. */
const now = Date.now()
const dev: Bot = { id: "dev", n: "Steve 开发", a: "", role: "", pers: "", pro: 1, h: "Codex", harness: "codex", m: "", model: null, on: true, acc: "manual", ws: "", project: null, perm: "", permMode: "auto", rule: "", prompt: null, mem: [], memCount: 0, memLoaded: true, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: [] }
const item = (id: string, t: string, x: Partial<Item>): Item => ({ id, t, b: "dev", st: "run", phase: "active", src: ["", ""], ws: "", project: null, up: "02:03", acc: "", goals: [], now: "", rounds: [], waits: [], ss: [], tl: [], outs: [], q: [], qs: null, branch: null, cwd: null, detail: false, dependents: [], created: now, updated: now, ...x })
const ask: Wait = { type: "ask", q: "迁移要在白天还是晚上做？", since: "3 分钟", decision: "d-1", subject: "action", session: null, options: ["白天", "晚上"], by: "dev" }
st.ready = true
st.version = "0.2.53"
st.bots = { dev }
st.items = new URLSearchParams(location.search).has("none") ? [item("ST-29", "已结束的旧单元", { st: "done", phase: "closed" })] : [
  item("ST-48", "修复执行进度重复展示", { now: "跑测试", plan: [["查构建日志", 1], ["修", 0]] }),
  item("ST-53", "选择数据库迁移方案", { st: "wait", wait: ask, waits: [ask] }),
  item("ST-7", "整理发布说明", { st: "pause", now: "写到一半" }),
  item("ST-10", "修复协作状态", { workflowPaused: true, workflowFailed: ["ST-43"] }),
  item("ST-29", "已结束的旧单元", { st: "done", phase: "closed" }),
]
function App() { useStore(); return <SidebarProvider><Sidebar route="items" /><SidebarInset className="relative min-w-0 overflow-hidden"><Page title="事项">正文</Page></SidebarInset></SidebarProvider> }
createRoot(document.getElementById("root")!).render(<App />)
