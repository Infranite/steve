import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ChatView } from "../../src/chat-view"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, emit, useStore, type Msg, type Bot } from "../../src/store"

document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const short: Msg[] = [
  { id: "hello", k: "me", t: "在吗", created: now - 180000 },
  { id: "hi", k: "steve", t: "在的。有什么需要我处理？", created: now - 170000 },
  { id: "request", k: "me", t: "帮我把对话页面整理得清晰一些，长回复也要好读。", created: now - 120000 },
  { id: "answer", k: "steve", t: "可以。我会整理消息层级、阅读间距和输入区，让每一轮对话更容易浏览。", created: now - 100000,
    steps: { tools: [], ms: 1800, route: { kind: "answer", title: "", bot: "steve", project: null, tags: [], newTags: [], why: "整理对话页的视觉层级和阅读体验", model: null, fallback: false, ms: 200 } } },
]
const long: Msg = { id: "long", k: "steve", created: now, replyTo: "request", t: "已整理好页面的视觉方案。\n\n### 这次调整\n\n- **消息层级**：正文、思考和引用分开呈现。\n- **阅读体验**：长回复保持舒适的行宽，手机上自然换行。\n- **输入区**：保留草稿、事项引用和停止生成。\n\n```ts\nconst conversation = { message: \"这是一段用于验证窄屏代码滚动的较长内容\", status: \"ready\" }\n```\n\n| 区域 | 调整 |\n| --- | --- |\n| 顶栏 | 固定高度 |\n| 消息 | 统一阅读栏 |\n\n下一步可以在浅色和深色主题中检查效果。" }
const steve: Bot = { id: "steve", main: true, n: "Steve", a: "", role: "你的助手", pers: "", pro: 1, h: "Codex", harness: "codex", m: "", model: null, on: true, acc: "manual", ws: "", project: null, perm: "", permMode: "auto", rule: "", prompt: null, mem: [], memCount: 0, memLoaded: true, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: [] }
st.bots = { steve }
st.thread = location.search.includes("empty") ? [] : location.search.includes("long") ? [...short, long] : short
st.notices = []
let replies = 0
window.addEventListener("fixture-reply", () => { replies++; st.thread = [...st.thread, { id: "reply-" + replies, k: "steve", created: Date.now(), t: "新到的一条回复。\n\n- 第一点\n- 第二点\n- 第三点" }]; emit() })
window.addEventListener("fixture-thinking", () => { st.thread = [...short, { k: "thinking" }]; st.thinking = true; st.phase = "answer"; emit() })
function Fixture() {
  useStore()
  return <SidebarProvider><main style={{ height: "100dvh", width: "100%", minWidth: 0 }}><ChatView /></main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
