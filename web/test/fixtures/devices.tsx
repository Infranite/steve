import { createRoot } from "react-dom/client"
import { StrictMode } from "react"
import "../../src/index.css"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { DevicesRoute } from "../../src/devices"
import { setPageWorkspace } from "../../src/api"
import { st } from "../../src/store"
import { getSheet, useRoute } from "../../src/ui"
import { useStore } from "../../src/store"
import { AddDeviceDialog } from "../../src/devices-add"

setPageWorkspace("fixture-workspace")
st.owner = !location.search.includes("member")
st.version = "0.2.112"
if (!location.hash) location.hash = "#/devices/nodes"
/* the devices pages under their routes, as the app mounts them (a route change remounts the page) */
function Routed() {
  const [, b, c, d] = useRoute()
  return <DevicesRoute key={`${b}/${c}`} b={b} c={c} d={d} />
}
/* the Add device dialog, as the app opens it from a sheet */
function Sheet() {
  useStore()
  const sh = getSheet()
  return sh?.kind === "addDevice" ? <AddDeviceDialog key={JSON.stringify(sh.payload ?? {})} {...(sh.payload ?? {})} /> : null
}
const content = <SidebarProvider><Routed /><Sheet /></SidebarProvider>
createRoot(document.getElementById("root")!).render(location.search.includes("strict") ? <StrictMode>{content}</StrictMode> : content)
