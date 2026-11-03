import { StrictMode, useState } from "react"
import { createRoot } from "react-dom/client"
import { Toaster } from "sonner"
import "../../src/index.css"
import { CleanupDialog, DisconnectDialog } from "../../src/integrations/disconnect"
function Fixture() {
  const [state, setState] = useState("open")
  const multi = new URLSearchParams(location.search).get("mode") === "multi"
  return <><main data-fixture-state={state}>{state}</main><Toaster />{state === "open" && (multi
    ? <DisconnectDialog title="断开测试接入" name="测试接入" leaving={[{ id: "alpha", label: "用户身份", stops: "停止同步" }, { id: "beta", label: "机器人身份", stops: "停止收消息" }]} onClose={() => setState("closed")} onDone={() => setState("done")} />
    : <CleanupDialog name="测试接入" target={{ connection_id: "alpha" }} onClose={() => setState("closed")} onDone={() => setState("done")} />)}</>
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
