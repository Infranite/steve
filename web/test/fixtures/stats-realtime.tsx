/* Browser fixture: the real stats page, with a test-only unmount control. API responses are supplied by the browser. */
import { StrictMode, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { StatsView } from "../../src/stats/page"
import { statsSeed } from "../../src/api/stats"
import { st } from "../../src/store"

const q = new URLSearchParams(location.search)
if (q.get("theme") === "dark") document.documentElement.classList.add("dark")
if (q.has("past")) statsSeed.next = { range: "custom", from: "2020-01-01", to: "2020-01-07" }
st.projects = [{ id: "p1", name: "Project Alpha" }, { id: "p2", name: "Project Beta" }] as typeof st.projects
st.bots = { dev: { id: "dev", n: "Developer" }, rev: { id: "rev", n: "Reviewer" } } as typeof st.bots

function Fixture() {
  const [mounted, setMounted] = useState(true)
  return <TooltipProvider delay={0}><SidebarProvider><main className="flex h-dvh w-full min-w-0 flex-col">
    <div className="flex flex-wrap items-center gap-2 bg-muted p-1 type-footnote" data-fixture-banner>
      <span>API fixture · synthetic contract data</span>
      <button type="button" data-fixture-unmount onClick={() => setMounted((v) => !v)}>Toggle stats view</button>
    </div>
    <div className="min-h-0 flex-1">{mounted && <StatsView />}</div>
  </main></SidebarProvider></TooltipProvider>
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
