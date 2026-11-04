/* ST-50: a long main conversation. The page has the latest 200 messages; the test serves the 300 before them from
 * /api/thread/history and /api/thread/search (see chat-history.browser.mjs). */
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ChatView } from "../../src/chat-view"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, emit, useStore, type Msg, type Bot } from "../../src/store"
import { created, older, text } from "./chat-history-data"

document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const steve: Bot = { id: "steve", main: true, n: "Steve", a: "", role: "你的助手", pers: "", pro: 1, h: "Codex", harness: "codex", m: "", model: null, on: true, acc: "manual", ws: "", project: null, perm: "", permMode: "auto", rule: "", prompt: null, mem: [], memCount: 0, memLoaded: true, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: [] }
st.bots = { steve }
st.workspace = { id: "ws-history", name: "测试", item_prefix: "ST" } as typeof st.workspace
st.notices = []
st.thread = Array.from({ length: 200 }, (_, n): Msg => { const i = older + n; return { id: "w" + i, k: i % 2 ? "steve" : "me", t: text(i), created: created(i) } })
st.ready = true
let replies = 0
window.addEventListener("fixture-reply", () => { replies++; st.thread = [...st.thread, { id: "reply-" + replies, k: "steve", created: Date.now(), t: "新到的一条回复。" }]; emit() })
function Fixture() {
  useStore()
  return <SidebarProvider><main style={{ height: "100dvh", width: "100%", minWidth: 0 }}><ChatView /></main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
