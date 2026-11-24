import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { stats } from "../../src/api/stats"
import { api, type Shell } from "../../src/api"
import type { DecisionView, ItemView, SessionView } from "../../src/gen"
import { SidebarProvider, SidebarInset } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { setLang } from "../../src/i18n"
import { refresh, useStore } from "../../src/store"
import { SessionsView } from "../../src/sessions-list"
import { fixtureProject, fixtureRawBot, fixtureShell, seedDecisionViews, seedItemViews, seedSessionViews } from "./view-data"
import { decisionView, itemView, sessionView } from "./wire-views.ts"

/* the 会话 list with a realistic spread: live, waiting, ended, imported and unlinked conversations across a week, on
 * several models and two bots. ?dark · ?en · ?narrow is left to the viewport */
const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark"))
if (q.has("en")) void setLang("en")
const now = Date.now(), H = 3_600_000
const titles = ["重设计多 Bot 协作区的信息展示", "修复完全放开后工具仍反复请求审批", "独立审查权限切换及隔离安全", "修复协作工作流状态检测与异常卡误报", "独立审查状态判定及回归覆盖", "统一工作状态判定与异常恢复", "整合协作区与关系图待确认设计", "统一执行体完整边界并修复关闭权限状态", "独立审查协作区与关系图设计", "修正执行证据、暂停结果与周期身份", "修订协作区与关系图统一设计", "补齐会话页的搜索高亮", "迁移旧版通知到信号中心", "Apple 客户端的同步策略", "整理事项列表的工具栏"]
const models = ["gpt-6.1-sol", "gpt-6.1-sol", "gpt-6.1-sol", "claude-opus-5-5", "gemini-4-carbon-high"]
const items: ItemView[] = []
const sessions: SessionView[] = []
for (let n = 0; n < 46; n++) {
  const no = 57 - n, id = "ST-" + no, title = titles[n % titles.length]
  const unlinked = n % 13 === 9, imported = n === 20 || n === 31
  if (!unlinked && !imported) items.push(itemView({
    id, no, title: n % 5 === 4 ? "统一工作状态判定（第 " + (n % 3 + 1) + " 轮）" : title,
    state: n < 3 ? { kind: "active" } : { kind: "closed", outcome: "finished", by: "owner" },
    bot_id: n % 4 === 2 ? "rev" : "dev", project_id: "p1", source: "chat",
    created: now - n * 3 * H, updated: now - n * 3 * H, goals: 1,
  }))
  const status = n === 0 || n === 2 ? "running" : n % 3 === 0 ? "ended" : "idle"
  sessions.push(sessionView({
    id: "s" + n, item_id: unlinked || imported ? undefined : id, no: n % 7 === 5 ? 2 : 1,
    bot_id: imported ? undefined : n % 4 === 2 ? "rev" : "dev",
    harness: imported ? "codex" : "claude-code", model: models[n % models.length], cwd: "/srv/code/steve", status,
    used: n % 6 === 1 ? undefined : [54, 77, 70, 81, 12, 28, 70, 60, 71, 67, 30, 34, 78, 74, 91][n % 15] * 2000, size: 200000,
    title: imported ? "本机 Codex：排查构建缓存" : title, device_id: "hub", source: imported ? "本机 Codex" : "Steve",
    readonly: imported, busy: status === "running",
    created: now - (n * 3.1 + 1) * H, updated: now - n * 3.1 * H - 7 * 60_000,
  }))
}
const decisions: DecisionView[] = [decisionView({
  id: "d1", kind: "approve", subject_type: "tool_call", subject_id: "{}", item_id: "ST-56", session_id: "s1",
  question: { kind: "raw", text: "允许执行：npm run build" }, options: ["允许一次", "拒绝"],
  requested_by: "dev", created: now - H,
})]
const state: Shell = fixtureShell({
  bots: [
    fixtureRawBot({ id: "dev", name: "开发 Bot", model: "gpt-6.1-sol", avatar: { color: "#e5484d" } }),
    fixtureRawBot({ id: "rev", name: "复核 Bot", model: "gpt-6.1-sol", avatar: { color: "#3e63dd" } }),
  ],
  projects: [fixtureProject({ path: "/srv/code/steve", default_bot: "dev" })],
})
api.shell = async () => structuredClone(state)
api.transcript = async () => ({ events: [], next: null })
api.searchSessions = async (s: string) => (s === "缓存" ? ["s20", "s4"] : [])
seedItemViews(items)
seedSessionViews(sessions)
seedDecisionViews(decisions)
/* a year of activity: quiet weekends, a busier last two months, a few heavy days */
stats.series = async (f, metric, _group, hourly) => {
  if (hourly) {
    /* one day by hour: a working day's shape, hours after now not yet come */
    const day = f.from!, now = new Date(), today = day === `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`
    let h0 = [...day].reduce((a, c) => a + c.charCodeAt(0), 0)
    const rnd = () => ((h0 = (h0 * 9301 + 49297) % 233280) / 233280)
    const values = Array.from({ length: 24 }, (_, h) => (today && h > now.getHours() ? null : h < 8 || h > 23 ? (rnd() < 0.1 ? 1 : 0) : rnd() < 0.6 ? Math.round(rnd() * 6) * (metric === "tokens" ? 180_000 : 1) : 0))
    return { bucket: "hour", buckets: values.map((_, h) => `${day}T${String(h).padStart(2, "0")}:00`), series: [{ key: "total", label: "", values }] }
  }
  const out: string[] = [], vals: number[] = []
  const d = new Date(f.from + "T00:00"), end = new Date(f.to + "T00:00")
  let seed = 7
  const rnd = () => ((seed = (seed * 9301 + 49297) % 233280) / 233280)
  for (let i = 0; d <= end; i++, d.setDate(d.getDate() + 1)) {
    const wk = d.getDay() === 0 || d.getDay() === 6, recent = (end.getTime() - d.getTime()) / 86_400_000 < 60
    const busy = rnd() < (wk ? 0.25 : 0.7) ? Math.round(rnd() * (recent ? 14 : 6)) + (recent ? 2 : 0) : 0
    out.push(`${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`)
    vals.push(metric === "tokens" ? busy * (200_000 + rnd() * 900_000) : busy)
  }
  return { bucket: "day", buckets: out, series: [{ key: "total", label: "", values: vals }] }
}
void refresh()
function Fixture() {
  useStore()
  return <TooltipProvider><SidebarProvider><SidebarInset className="relative h-dvh min-w-0 overflow-hidden"><SessionsView /></SidebarInset><Toaster /></SidebarProvider></TooltipProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
