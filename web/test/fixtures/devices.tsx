import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { DevicesView } from "../../src/devices"
import { setPageWorkspace } from "../../src/api"
import { st } from "../../src/store"

setPageWorkspace("fixture-workspace")
const device = { id: "hub", name: "Fixture Hub", roles: ["Hub"], os: "fixture", version: "fixture", status: "正常", max_sessions: 4, seen: Date.now(), created: 0 }
st.devices = [device, ...(location.search.includes("legacy") ? [{ ...device, id: "fixture-remote", name: "Legacy duplicate", roles: ["执行节点"] }] : [])]
st.runtimes = [{ id: "hub-only", name: "Hub-only harness", ok: true, version: "fixture", path: "/fixture/adapter", models: [], note: "fixture" }]
createRoot(document.getElementById("root")!).render(<SidebarProvider><DevicesView /></SidebarProvider>)
