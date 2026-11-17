import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { DevicesRoute } from "../../src/devices"
import { setPageWorkspace } from "../../src/api"
import { st } from "../../src/store"
import { getSheet, useRoute } from "../../src/ui"
import { useStore } from "../../src/store"
import { AddDeviceDialog } from "../../src/devices-add"

setPageWorkspace("fixture-workspace")
st.owner = true
st.version = "0.2.112"
const device = { id: "hub", name: "Fixture Hub", roles: ["Hub"], os: "fixture", version: "fixture", status: "正常", max_sessions: 4, seen: Date.now(), created: 0 }
st.devices = [device, ...(location.search.includes("legacy") ? [{ ...device, id: "fixture-remote", node_id: "fixture-remote", name: "Legacy duplicate", roles: ["执行节点"] }] : [])]
st.runtimes = [{ id: "hub-only", name: "Hub-only harness", ok: true, version: "fixture", path: "/fixture/adapter", models: [], note: "fixture" }]
if (!location.hash) location.hash = "#/devices/nodes"
/* the devices pages under their routes, as the app mounts them (a route change remounts the page) */
function Routed() {
  const [, b, c] = useRoute()
  return <DevicesRoute key={`${b}/${c}`} b={b} c={c} />
}
/* the Add device dialog, as the app opens it from a sheet */
function Sheet() {
  useStore()
  const sh = getSheet()
  return sh?.kind === "addDevice" ? <AddDeviceDialog key={JSON.stringify(sh.payload ?? {})} {...(sh.payload ?? {})} /> : null
}
createRoot(document.getElementById("root")!).render(<SidebarProvider><Routed /><Sheet /></SidebarProvider>)
