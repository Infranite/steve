import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ChatView } from "../../src/chat-view"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, msgOf, useStore, type Bot } from "../../src/store"

/* 对话里的配置改动卡片：一句话起草多处，其中一处已应用（状态来自消息的 cfg_state） */
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const quiet = { id: "chg_quiet", title: "设置 · 通知", target: { kind: "settings", id: "notify" }, href: "#/settings/notify", changes: [{ label: "免打扰", from: "23:00 – 08:00", to: "22:00 – 09:00" }, { label: "每天汇报", from: "08:30", to: "09:00" }] }
const dev = { id: "chg_dev", title: "Steve 开发 Bot 的设置", target: { kind: "bot", id: "dev" }, href: "#/bots/dev", changes: [{ label: "思考强度", from: "默认", to: "high" }, { label: "交付验收", from: "跟随全局", to: "自动验收" }] }
const yolo = { id: "cs_yolo", title: "YOLO 模式", target: { kind: "yolo", id: "default" }, href: "#/yolo", changes: [{ label: "YOLO", from: "关", to: "开" }] }
const raw = (id: string, text: string, meta: any) => ({ id, workspace_id: "default", author: "steve", text, item_id: null, created: now, meta, space: "steve" }) as any
const steve: Bot = { id: "steve", main: true, n: "Steve", a: "", role: "你的助手", pers: "", pro: 1, h: "Codex", harness: "codex", m: "", model: null, on: true, acc: "manual", ws: "", project: null, perm: "", permMode: "auto", rule: "", prompt: null, mem: [], memCount: 0, memLoaded: true, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: [] }
st.bots = { steve }
st.notices = []
st.thread = [
  { id: "ask", k: "me", t: "免打扰改成 22 点到 9 点，每天 9 点汇报；开发 Bot 思考强度调到 high、自动验收；再开 YOLO", created: now - 60000 },
  msgOf(raw("cfg", "这是配置改动，我起草好了 3 处，确认后分别应用：", { proposals: [quiet, dev, yolo], cfg_state: { cs_yolo: "applied" } })),
  { id: "q", k: "me", t: "现在免打扰是几点？", created: now - 20000 },
  { id: "now", k: "steve", t: "「设置 · 通知」现在是：\n- 免打扰：23:00 – 08:00\n- 每天汇报：08:30\n\n要改的话直接说改成什么，我起草好给你确认。", created: now - 10000 },
]
function Fixture() {
  useStore()
  return <SidebarProvider><main style={{ height: "100dvh", width: "100%", minWidth: 0 }}><ChatView /></main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
