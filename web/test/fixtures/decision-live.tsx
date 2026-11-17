/* Manual operation entry for an isolated backend. Uses the real API clients without overrides.
 * Start Vite with STEVE_HUB pointing at the isolated instance; never use production test messages. */
import { createRoot } from "react-dom/client"
import { useEffect } from "react"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { DecisionCard } from "@/ui"
import { refresh, st, useStore } from "@/store"
function App() {
  useStore()
  useEffect(() => { void refresh() }, [])
  return <main className="mx-auto flex max-w-[640px] flex-col gap-4 px-4 py-6">
    <button type="button" data-live-refresh onClick={() => void refresh()}>Refresh</button>
    {st.error && <p role="alert">{st.error}</p>}
    {st.orphans.map(({ i, w }) => <DecisionCard key={w.decision} i={i} showItem={false} />)}
    <Toaster />
  </main>
}
createRoot(document.getElementById("root")!).render(<App />)
