/* 设置的几个窗格（工作空间、数据、通知）：真实的 SettingsView，接口由浏览器测试拦截。?theme=dark 切深色。
 * window.__land(patch) stands in for the state refresh a save starts: it puts new Hub values into the store. */
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { SettingsView } from "../../src/manage"
import { emit, st, useStore } from "../../src/store"

if (new URLSearchParams(location.search).get("theme") === "dark") document.documentElement.classList.add("dark")
const ws = { id: "ws_1", name: "Steve", description: "开源智能体枢纽", item_prefix: "ST", sort: 0, created: 0 }
st.workspace = ws as typeof st.workspace
st.workspaces = [ws] as typeof st.workspaces
st.settings = { digestAt: "09:00", quiet: "关闭" }
st.prefix = "ST"
st.noSeq = 43
Object.assign(window, {
  __land: (p: { workspace?: Partial<typeof ws>; settings?: Record<string, string> }) => {
    if (p.workspace) { st.workspace = { ...st.workspace!, ...p.workspace }; st.workspaces = [st.workspace] }
    if (p.settings) st.settings = { ...st.settings, ...p.settings }
    emit()
  },
})
function Fixture() {
  useStore()
  return <SidebarProvider><main style={{ width: "100%", minWidth: 0 }}><SettingsView /></main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<StrictMode><TooltipProvider delay={0}><Fixture /></TooltipProvider></StrictMode>)
