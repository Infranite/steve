import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ItemCard } from "../../src/thread/parts"
import { refresh, useStore } from "../../src/store"
import { bindPageHub } from "../../src/hub"

bindPageHub("")
void refresh()
function Fixture() {
  useStore()
  return <main className="mx-auto max-w-2xl p-4"><ItemCard c={{ item: "ST-100", state: "done", delivery: { text: "Delivery browser fixture", at: 1 } }} /></main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
