/* 设置 · 外观 with the real store: the type size and reduced motion go to the Hub (the browser test answers /api/settings
 * and /api/state) and come back through the state refresh, which puts them on <html>. ?theme=dark switches to dark. */
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { applyStoredLook } from "../../src/lib/motion"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { SettingsView } from "../../src/manage"
import { refresh, useStore } from "../../src/store"

if (new URLSearchParams(location.search).get("theme") === "dark") document.documentElement.classList.add("dark")
applyStoredLook()
void refresh()
function Fixture() {
  useStore()
  return <SidebarProvider><main style={{ width: "100%", minWidth: 0 }}>
    <p className="type-body" data-sample-body>正文 Body</p>
    <p className="text-sm" data-sample-sm>text-sm</p>
    <button type="button" data-sample-button>按钮</button>
    <SettingsView />
  </main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<StrictMode><TooltipProvider delay={0}><Fixture /></TooltipProvider></StrictMode>)
