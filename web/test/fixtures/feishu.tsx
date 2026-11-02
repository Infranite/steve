import { StrictMode, useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { FeishuGuide, SelfButtons } from "../../src/integrations/feishu"

const existing = new URLSearchParams(location.search).has("existing")
function RaceFixture() {
  const [app, setApp] = useState("app_a")
  useEffect(() => { const swap = () => setApp("app_b"); window.addEventListener("fixture-switch-app", swap); return () => window.removeEventListener("fixture-switch-app", swap) }, [])
  return <div data-fixture-app={app}><SelfButtons app={app} /></div>
}
const race = new URLSearchParams(location.search).has("race")
createRoot(document.getElementById("root")!).render(<StrictMode>
  <main style={{ maxWidth: 760, margin: "auto", padding: 20 }}>
    {race ? <RaceFixture /> : <FeishuGuide adapters={[]} bots={existing ? [{ id: "con_fixture", name: "测试机器人", domain: new URLSearchParams(location.search).has("lark") ? "lark" : "feishu" }] : []} selfDone={false} callbackUrls={new URLSearchParams(location.search).has("callback") ? ["https://steve.example.test/auth/connect/callback"] : []} />}
  </main>
</StrictMode>)
