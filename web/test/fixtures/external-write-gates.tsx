import { useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { AccountsRoute } from "../../src/accounts"
import { IntegrationsRoute } from "../../src/integrations"

function Fixture() {
  const [hash, setHash] = useState(location.hash)
  useEffect(() => {
    const change = () => setHash(location.hash)
    window.addEventListener("hashchange", change)
    return () => window.removeEventListener("hashchange", change)
  }, [])
  const [, route, id] = hash.split("/")
  return <SidebarProvider><main className="mx-auto h-svh w-full max-w-3xl overflow-hidden">
    {route === "integrations" ? <IntegrationsRoute id={id} /> : <AccountsRoute route="accounts" id={id} />}
  </main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
