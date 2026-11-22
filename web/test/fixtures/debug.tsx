import { useState } from "react"
import { createRoot } from "react-dom/client"
import { Toaster } from "@/components/ui/toaster"
import "../../src/index.css"
import { api, type RItemDetail, type Shell } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useSession, useStore, type Sess } from "../../src/store"
import { ItemView } from "../../src/views"
import SessionView from "../../src/session/view"
import { seedItems, seedSessions } from "./view-data"
import { setLang } from "../../src/i18n"

setLang(new URLSearchParams(location.search).get("lang") === "en" ? "en" : "zh-CN")
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const item = {
  id: "ST-123", workspace_id: "default", no: 123, title: "检查卡片重复出现的原因", status: "block", stage: "执行", bot_id: "dev", project_id: null,
  cwd: null, branch: null, plan: [], now: "等待前置事项", accept_mode: null, priority: "中", archived: false, star: false, due: null, snooze: null,
  source: ["手动新建", "manual"], parent: null, created: now - 10_000, updated: now, goal: "根据具体事件定位原因", goals: 1, tags: [], deps: [],
  sessionIds: ["session-one", "session-two"], open: [], queued: 0, live: false,
}
const detail: RItemDetail = { ...item, status: "block", goalsList: [{ version: 1, text: item.goal, created: now }], timeline: [], artifacts: [], decisions: [], dependents: [] }
const session = {
  item: item.id, b: "dev", harness: "codex", cwd: "/tmp/example", st: "idle",
  title: "等待前置", device: "hub", src: "item", ro: true, since: now, at: now,
} satisfies Partial<Sess>
const state = {
  version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [],
  allBots: [], bots: [{ id: "dev", name: "开发 Bot", role: "", main: false, harness: "codex", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [], tags: [], tagUses: {}, thread: [], settings: {}, devices: [], runtimes: [],
} as unknown as Shell
api.shell = async () => structuredClone(state)
api.item = async () => structuredClone(detail)
api.transcript = async () => ({ events: [], next: null })
api.queued = async () => []
seedItems([{ id: item.id, t: item.title, st: "block", b: item.bot_id, src: ["手动新建", "manual"] }])
seedSessions({ "session-one": { ...session, n: 1 }, "session-two": { ...session, n: 2 } })
void refresh()
function Fixture() {
  useStore()
  const [session, setSession] = useState(false)
  const view = useSession(session ? "session-two" : null)
  return <TooltipProvider><SidebarProvider><main className="h-screen w-full">
    <button className="m-2 rounded border px-2 py-1" onClick={() => setSession(!session)} data-fixture-switch>{session ? "Item" : "Session"}</button>
    {session ? <SessionView k="session-two" s={view} /> : <ItemView id="ST-123" />}
  </main><Toaster /></SidebarProvider></TooltipProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
