import { fixtureItemDetail, fixtureRawBot, fixtureShell, seedItems, seedSessions } from "./view-data"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { api, type REvent, type RItemDetail, type Shell } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useStore } from "../../src/store"
import { ItemView } from "../../src/views"

/* ST-42: an item page fed through the store as the Hub sends it. Session #1 ended, then a new request started session #2,
 * which runs; a comment to #2 is being answered. Plan changes are written while the sessions run. */
document.documentElement.classList.toggle("dark", location.search.includes("dark"))
const m = (n: number) => Date.UTC(2026, 10, 5, 0, n)
const sess = (id: string, no: number, status: "running" | "ended" | "idle", created: number, updated: number) => ({ id, item_id: "ST-9", no, bot_id: "dev", harness: "claude-code", model: "claude-opus-5-5", cwd: "/tmp/st-9", status, used: 36000, size: 200000, title: "会话中间过程事件排序修正", device_id: "hub", source: "item", readonly: false, busy: status === "running", created, updated })
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
const ev = (n: number, kind: string, data: Record<string, unknown>): REvent => ({ seq: ++seq, ts: m(n), kind, data })
const tool = (n: number, title: string): REvent => ev(n, "tool", { tool: { id: "t" + seq, origin: "harness", name: "Bash", category: "exec", title, target: title, phase: "succeeded" } })
const events: Record<string, REvent[]> = {
  s1: [ev(8, "user", { text: "修复过程事件的显示顺序", via: "steve" }), tool(9, "运行 npm run check"), ev(19, "text", { role: "assistant", text: "做完了，等你验收。" }), ev(20, "turn_end", { stopReason: "end_turn" })],
  s2: [ev(30, "user", { text: "补充信息：再加一个需求：深色模式下也要检查", via: "steve" }), tool(31, "运行 npm run check"), ev(33, "text", { role: "assistant", text: "深色模式查过了。" }), ev(34, "turn_end", { stopReason: "end_turn" }),
    ev(39, "user", { text: "补充信息：顺便看一下手机端", via: "me" }), tool(40, "运行 npm run test:web"), tool(41, "运行 sed -i '' 's/a/b/' src/views.tsx")],
}
const detail: RItemDetail = fixtureItemDetail({
  id: "ST-9", source: ["对话", "thread"], plan: [{ step: "修正排序", done: false }],
  goalsList: [{ version: 1, text: "会话的动态部分始终在最下方", created: m(7) }], timeline,
})
const state: Shell = fixtureShell({
  bots: [fixtureRawBot({ id: "dev", name: "Steve 开发 Bot" })],
})
api.shell = async () => structuredClone(state)
api.item = async () => structuredClone(detail)
api.transcript = async (id) => ({ events: events[id] ?? [], next: null })
seedSessions(Object.fromEntries(sessions.map((s) => [s.id, { item: s.item_id ?? null, b: s.bot_id, n: s.no, harness: s.harness, m: s.model, title: s.title, cwd: s.cwd, st: s.status === "running" ? "run" : s.status === "ended" ? "ended" : "idle", since: s.created, at: s.updated, busy: s.busy, ctxUsed: s.used ?? undefined, ctxSize: s.size ?? undefined }])))
seedItems([{ id: "ST-9", t: "会话中间过程事件排序修正", st: "run", b: "dev", src: ["对话", "thread"], plan: [["修正排序", 0]], created: m(7), updated: m(41) }])
void refresh()
function Fixture() { useStore(); return <TooltipProvider><SidebarProvider><main className="w-full"><ItemView id="ST-9" /></main><Toaster /></SidebarProvider></TooltipProvider> }
createRoot(document.getElementById("root")!).render(<Fixture />)
