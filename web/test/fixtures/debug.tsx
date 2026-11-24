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
import { fixtureItemDetail, fixtureRawBot, fixtureShell, seedItems, seedSessions } from "./view-data"
import { setLang } from "../../src/i18n"

setLang(new URLSearchParams(location.search).get("lang") === "en" ? "en" : "zh-CN")
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const detail: RItemDetail = fixtureItemDetail({
  id: "ST-123", source: ["手动新建", "manual"],
  goalsList: [{ version: 1, text: "根据具体事件定位原因", created: now }],
})
const session = {
  item: detail.id, b: "dev", harness: "codex", cwd: "/tmp/example", st: "idle",
  title: "等待前置", device: "hub", src: "item", ro: true, since: now, at: now,
} satisfies Partial<Sess>
const state: Shell = fixtureShell({
  bots: [fixtureRawBot({ id: "dev", name: "开发 Bot", harness: "codex" })],
})
api.shell = async () => structuredClone(state)
api.item = async () => structuredClone(detail)
api.transcript = async () => ({ events: [], next: null })
api.queued = async () => []
seedItems([{ id: detail.id, t: "检查卡片重复出现的原因", st: "block", b: "dev", src: ["手动新建", "manual"], created: now - 10_000, updated: now }])
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
