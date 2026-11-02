import { StrictMode, useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ChatView } from "../../src/views"
import { SpaceView } from "../../src/channels/space"
import { SidebarProvider } from "../../src/components/ui/sidebar"
function Fixture() {
  const [space, setSpace] = useState("space")
  useEffect(() => { const switchSpace = () => setSpace("other"); window.addEventListener("fixture-switch", switchSpace); return () => window.removeEventListener("fixture-switch", switchSpace) }, [])
  return <SidebarProvider><main style={{ height: "100vh", width: "100%" }}>{location.search.includes("recent") ? <SpaceView id={space} /> : <ChatView conv={space} />}</main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
