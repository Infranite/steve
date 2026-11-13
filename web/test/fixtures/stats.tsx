/* 统计页夹具（ST-40）：真实的 StatsView，事项和 YOLO 状态直接写进 store，统计接口由浏览器测试拦截。
 * ?theme=dark 切深色，?yolo=paused|off|full 切运行状态，?view=inbox 看待处理页（熔断暂停时顶上那一条），
 * ?view=item&id=ST-1 看一件事项的下钻（费用去向）。 */
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, emit, useStore, type Item, type Wait } from "../../src/store"
import { getSheet } from "../../src/ui"
import { StatsView } from "../../src/stats/page"
import { StatsItemView } from "../../src/stats/drill"
import { InboxView } from "../../src/views"

const q = new URLSearchParams(location.search)
if (q.get("theme") === "dark") document.documentElement.classList.add("dark")
const now = Date.now()
const wait = (type: Wait["type"], text: string, id: string): Wait => ({ type, q: text, since: "", decision: id, subject: "item", session: null, options: [], by: "dev" })
const item = (no: number, status: Item["st"], title: string, x: Partial<Item> = {}): Item => ({
  id: `ST-${no}`, t: title, b: "dev", st: status, src: ["对话", "chat"], ws: "Steve", project: "p1", up: "", acc: "跟随全局", goals: [], now: "正在推进", rounds: [], waits: [], ss: [], tl: [], outs: [], q: [], qs: null,
  branch: null, cwd: null, detail: false, dependents: [], created: now - no * 3600000, updated: now - no * 600000, ...x,
} as Item)
const fail = wait("fail", "会话出错：构建脚本退出 1", "d_fail"), accept = wait("accept", "验收：统计页重整", "d_accept")
st.items = [
  item(1, "run", "重整统计页图表"), item(2, "run", "整理发布脚本"), item(3, "run", "修复构建失败", { waits: [fail], wait: fail }),
  item(4, "wait", "统计页验收", { waits: [accept], wait: accept }), item(5, "prop", "提议：清理旧日志"),
  item(6, "block", "等 ST-2 发布后再迁移", { deps: ["ST-2"] }), item(7, "pause", "暂停的调研"),
  item(8, "done", "已完成的事项"), item(9, "drop", "放弃的事项"), item(10, "run", "归档的事项", { arch: true }),
]
st.projects = [{ id: "p1", name: "Steve" } as any]
st.bots = { dev: { id: "dev", n: "Steve 开发 Bot" } as any }
const y = q.get("yolo") ?? "full"
st.yolo = y === "none" ? null : {
  on: y !== "off", mode: y === "full" ? "full" : y === "off" ? "off" : "on", full: y === "full", active: y !== "off" && y !== "paused", forced_off: null, forced_reason: null,
  paused: y === "paused" ? { code: "undo_burst", reason: "1 小时内撤销了 2 次", at: now } : null, window: { id: "w1", started: now - 3600000, include_open: true }, scopes: [],
  counts: { decided: 3, declined: 1, scheduled: 0 },
}
emit()

function Fixture() {
  useStore()
  return <TooltipProvider delay={0}><SidebarProvider><main style={{ height: "100dvh", width: "100%", minWidth: 0 }}>{q.get("view") === "inbox" ? <InboxView /> : q.get("view") === "item" ? <StatsItemView id={q.get("id") ?? "ST-1"} /> : <StatsView />}</main><output hidden data-sheet={getSheet()?.kind ?? ""} data-hash={location.hash} /></SidebarProvider></TooltipProvider>
}
window.addEventListener("hashchange", () => emit())
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
