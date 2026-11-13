import { useState } from "react"
import { createRoot } from "react-dom/client"
import { Toaster } from "@/components/ui/toaster"
import "../../src/index.css"
import { api, type RItemDetail, type RSession, type State } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useStore } from "../../src/store"
import { ItemView, SessionView } from "../../src/views"
import { setLang } from "../../src/i18n"

setLang(new URLSearchParams(location.search).get("lang") === "en" ? "en" : "zh-CN")
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const item = {
  id: "ST-123", workspace_id: "default", no: 123, title: "检查卡片重复出现的原因", status: "block", stage: "执行", bot_id: "dev", project_id: null,
  cwd: null, branch: null, plan: [], now: "等待前置事项", accept_mode: null, priority: "中", archived: false, star: false, due: null, snooze: null,
  source: ["手动新建", "manual"], parent: null, created: now - 10_000, updated: now, goal: "根据具体事件定位原因", goals: 1, tags: [], deps: [],
  sessionIds: ["session-one", "session-two"], open: [], queued: 0, live: false,
} as unknown as RItemDetail
const detail: RItemDetail = { ...item, goalsList: [{ version: 1, text: item.goal, created: now }], timeline: [], artifacts: [], decisions: [], dependents: [] }
const sessions = ["session-one", "session-two"].map((id, n): RSession => ({
  id, workspace_id: "default", item_id: item.id, no: n + 1, bot_id: "dev", harness: "codex", model: null, cwd: "/tmp/example", status: "idle", used: null,
  size: null, cost: null, title: "等待前置", device_id: "hub", source: "item", readonly: true, busy: false, queued: [], created: now, updated: now,
}))
const state = {
  version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [],
  items: [item], decisions: [], recent: [], allBots: [], bots: [{ id: "dev", name: "开发 Bot", role: "", main: false, harness: "codex", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions, devices: [], runtimes: [],
} as unknown as State
api.state = async () => structuredClone(state)
api.item = async () => structuredClone(detail)
api.events = async () => []
void refresh()
function Fixture() {
  useStore()
  const [session, setSession] = useState(false)
  return <TooltipProvider><SidebarProvider><main className="h-screen w-full">
    <button className="m-2 rounded border px-2 py-1" onClick={() => setSession(!session)} data-fixture-switch>{session ? "Item" : "Session"}</button>
    {session ? <SessionView k="session-two" /> : <ItemView id="ST-123" />}
  </main><Toaster /></SidebarProvider></TooltipProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
