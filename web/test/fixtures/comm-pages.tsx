import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ChannelsRoute } from "../../src/channels/channels"
import { IntegrationsRoute } from "../../src/integrations/index"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { useStore } from "../../src/store"

/* `?view=list|space|account|integrations` (and `&dark`): the real pages over the mocked API of comm-pages.browser.mjs */
document.documentElement.classList.toggle("dark", location.search.includes("dark"))
const view = new URLSearchParams(location.search).get("view") ?? "list"
function Fixture() {
  useStore()
  const page = view === "space" ? <ChannelsRoute b="space" id="sp_ops" /> : view === "account" ? <ChannelsRoute b="acc" id="conn_me" /> : view === "integrations" ? <IntegrationsRoute /> : view === "provider" ? <IntegrationsRoute id="mail" /> : <ChannelsRoute />
  return <SidebarProvider><main style={{ width: "100%", minWidth: 0 }}>{page}</main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
