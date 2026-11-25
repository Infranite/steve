import { fixtureBot, seedMessages } from "./view-data"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ChatView } from "../../src/chat-view"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, useStore, hubSubs, type Msg } from "../../src/store"

/* `?main`: your own conversation with a follow-up question; otherwise the IM conversation `chan:sp1` from the mocked API */
document.documentElement.classList.toggle("dark", location.search.includes("dark"))
const now = Date.now()
const main: Msg[] = [
  { id: "q", k: "me", t: "帮我看下这条", created: now - 60000 },
  { id: "ask", k: "steve", t: "这条消息看起来是发给林知远的，你是想让我帮忙处理吗？", created: now - 50000, ask: { options: ["这条是发给林知远的，我不用处理", "请补充完整"] } },
]
st.bots = { steve: fixtureBot({ id: "steve", main: true, n: "Steve", role: "你的助手" }) }
seedMessages(location.search.includes("main") ? main : [])
/* the Hub saying a chat message of the space changed, as its event stream would */
window.addEventListener("fixture-reload", () => hubSubs.forEach((f) => f({ rev: 1, type: "chan_message", id: "cm_out", space_id: "sp1", ws: "default" })))
function Fixture() {
  useStore()
  return <SidebarProvider><main style={{ height: "100dvh", width: "100%", minWidth: 0 }}>{location.search.includes("main") ? <ChatView /> : <ChatView conv="chan:sp1" />}</main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
