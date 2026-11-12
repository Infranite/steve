/* Real controls and dialogs; the browser intercepts every API request, never a live workspace. */
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { useState } from "react"
import { hubSubs, st, useStore, type Item } from "../../src/store"
import { getSheet } from "../../src/ui"
import { YoloDialog, YoloPill } from "../../src/yolo/switch"
import { YoloItemModes, YoloItemSwitch, YoloProjectSelect } from "../../src/yolo/badges"

const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.get("theme") === "dark")
st.yolo = {
  on: q.get("state") === "paused", mode: q.get("state") === "paused" ? "on" : "off", active: false,
  forced_off: q.get("state") === "forced" ? "env" : null,
  forced_reason: q.get("state") === "forced" ? "configuration/".repeat(24) : null,
  paused: q.get("state") === "paused" ? { code: "fixture", reason: "reason/".repeat(30), at: Date.now() } : null,
  window: null, scopes: [], counts: { decided: 0, declined: 0, scheduled: 0 },
}
const items = ["ST-100", "ST-101"].map((id) => ({ id, project: "p1", yolo: null }) as unknown as Item)
/* the browser test's stand-in for the Hub's event stream */
;(window as any).__hubEvent = (e: any) => hubSubs.forEach((f) => f(e))
function Fixture() {
  useStore()
  const [n, setN] = useState(0)
  const item = items[n]
  return <main className="p-4">
    <YoloPill />
    {q.has("swap") && <button type="button" data-yolo-swap onClick={() => setN(1 - n)}>{item.id}</button>}
    <YoloItemSwitch i={item} />
    <YoloItemModes i={item} />
    <YoloProjectSelect id="p1" realDir={false} />
    {getSheet()?.kind === "yolo" && <YoloDialog />}
  </main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
