import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { api, type RNode } from "../../src/api"
import { PairDialog } from "../../src/devices"
const n: RNode = { node_id: "fixture-node", name: "测试设备", fingerprint: "sha256:fixture-fingerprint", state: "pending", linked: false, local: false }
api.nodes = async () => ({ nodes: [{ ...n }] })
api.createPairing = async () => ({ code: "fixture-code-not-a-credential", expires: Date.now() + (location.search.includes("expired") ? -1 : 600000), pin: "sha256:fixture-hub", hub_id: "fixture-hub" })
api.confirmNode = async id => {
  if (id !== n.node_id) throw new Error("wrong node")
  if (location.search.includes("denied")) throw new Error("fixture: owner confirmation denied")
  n.state = "active"; return { ok: true }
}
createRoot(document.getElementById("root")!).render(<PairDialog />)
