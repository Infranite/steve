import { StrictMode, useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ConvSwitcher } from "../../src/thread/conversations"
function Fixture() {
  const [current, setCurrent] = useState<string>()
  useEffect(() => { const sync = () => setCurrent(location.hash.startsWith("#/c/") ? location.hash.slice(4) : undefined); sync(); window.addEventListener("hashchange", sync); return () => window.removeEventListener("hashchange", sync) }, [])
  return <ConvSwitcher current={current} />
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
