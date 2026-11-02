import { StrictMode, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { QueueStrip } from "../../src/queue"
import type { RQueued } from "../../src/api"
const row = (id: string, via: string, text: string): RQueued => ({ id, via, text, created: 1, files: [] })
function Fixture() {
  const [resumed, setResumed] = useState(false)
  return <main style={{ maxWidth: 760, margin: "auto", padding: 20 }}>
    <QueueStrip sid="fixture" note="本回合结束后发送" rows={[
      row("normal", resumed ? "resumed:user" : "user", "普通排队消息"),
      row("recovery", "resumed:user", "重启前的消息"),
      row("agent", "resumed:steve", "重启前的 Agent 消息"),
    ]} />
    <ButtonFixture onClick={() => setResumed(true)} />
  </main>
}
function ButtonFixture({ onClick }: { onClick: () => void }) { return <button onClick={onClick}>模拟队列恢复标记</button> }
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
