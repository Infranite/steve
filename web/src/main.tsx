import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import "./i18n/index.ts"
import "@fontsource-variable/geist"
import "@fontsource-variable/geist-mono"
import "./index.css"
import App from "./App.tsx"
import { applyStoredLook } from "@/lib/motion"
import { deliveryRoute } from "@/delivery"
import { lazyView } from "@/lazy"
import { bindPageHub } from "@/hub"
import { modReq, setPageWorkspace } from "@/api"
import { startPerf } from "@/perf"

/* the type size and reduced motion this device saw last, until the Hub's settings arrive */
applyStoredLook()

const DeliveryPage = lazyView(() => import("@/delivery-page").then((m) => ({ default: m.DeliveryPage })))
const delivery = deliveryRoute(location.pathname)
const workspace = new URLSearchParams(location.search).get("ws")
if (delivery || workspace) {
  bindPageHub("")
  if (!delivery?.shared && workspace) setPageWorkspace(workspace)
}
// Share pages must not start the authenticated application, its state requests or its event stream.
if (!delivery?.shared) startPerf((r) => modReq("POST", "/system/web-perf", r))
createRoot(document.getElementById("root")!).render(<StrictMode>{delivery?.shared ? <DeliveryPage id={delivery.id} shared /> : <App deliveryId={delivery?.id} />}</StrictMode>)
