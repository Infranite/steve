import { StrictMode, useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { st } from "../../src/store"
import { ConvSwitcher, openConversations } from "../../src/thread/conversations"
function Fixture() {
  const [current, setCurrent] = useState<string>()
  useEffect(() => { const sync = () => setCurrent(location.hash.startsWith("#/c/") ? location.hash.slice(4) : undefined); sync(); window.addEventListener("hashchange", sync); return () => window.removeEventListener("hashchange", sync) }, [])
  const [scope, setScope] = useState("ws-a")
  useEffect(() => { const change = () => { st.workspace = { ...st.workspace, id: "ws-b" } as typeof st.workspace; setScope("ws-b") }; window.addEventListener("fixture-workspace", change); return () => window.removeEventListener("fixture-workspace", change) }, [])
  /* the history panel opens the list (ST-50); this stands in for its row */
  return <><button type="button" onClick={openConversations} data-open-conversations>打开群聊和私聊</button><ConvSwitcher key={scope} current={current} /></>
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
