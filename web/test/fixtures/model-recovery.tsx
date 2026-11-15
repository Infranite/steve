import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { api } from "../../src/api"
import { ChatView } from "../../src/chat-view"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, emit, type Bot } from "../../src/store"

document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
st.owner = true
st.onboard = -1
st.workspace = { id: "w1", name: "个人" } as typeof st.workspace
st.bots = { steve: { id: "steve", n: "Steve", main: true, harness: "claude-code", h: "Claude Code", model: "old-model", m: "old-model", mem: [], memCount: 0, routines: [], color: "#8b9eac", round: 60 } as unknown as Bot }
st.runtimes = [{id:"claude-code", name:"Claude Code"}, {id:"codex", name:"Codex"}] as typeof st.runtimes
st.settings = { onboard: "pending" }
st.thread = [
  { id: "question", k: "me", t: "用来构建 AI Native 的生活与工作", files: [{id:"file-1",name:"context.txt",mime:"text/plain",size:42} as any], created: Date.now() - 2000 },
  { id: "failed", k: "steve", origin: "question", t: "API Error: 503 no eligible upstream account is currently available", created: Date.now() - 1000, steps: { tools: [], research: { run: "run1", model: "old-model", ms: 1000, turns: 1, tools: 0, stopped: "failed" } } },
]
// A current focus and a different draft must never redirect a retry of the failed message.
st.focus = { item: "ST-99", until: Date.now() + 600000 }
;(window as any).__say = []
api.say = async (...args) => { (window as any).__say.push(args); return {} as never }
window.addEventListener("fixture-scope", () => { st.workspace = { ...st.workspace, id: "w2" } as typeof st.workspace; emit() })
createRoot(document.getElementById("root")!).render(<SidebarProvider><main style={{height:"100dvh",width:"100%",minWidth:0}}><ChatView /></main></SidebarProvider>)
