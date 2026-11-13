import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { AutoRoute } from "../../src/auto/automation"
import { SignalsView } from "../../src/signals"
import { SidebarInset, SidebarProvider } from "../../src/components/ui/sidebar"
import { useRoute } from "../../src/ui"
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
/* #/auto/rules, #/auto/schedules, #/auto/rule/<id>: the page App.tsx renders for "auto"; #/signals: 信号 */
function Auto() { const [a, b, c] = useRoute(); return a.split("?")[0] === "signals" ? <SignalsView /> : <AutoRoute b={b} c={c} /> }
createRoot(document.getElementById("root")!).render(
  <SidebarProvider><SidebarInset className="relative min-w-0 overflow-hidden"><div className="min-h-0 flex-1"><Auto /></div></SidebarInset><Toaster /></SidebarProvider>)
