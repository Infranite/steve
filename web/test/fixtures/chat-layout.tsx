import { seedItems } from "./view-data"
import { useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ChatView } from "../../src/chat-view"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, emit, useStore, hubSubs, type Msg, type Bot, type Item } from "../../src/store"

document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const short: Msg[] = [
  { id: "hello", k: "me", t: "在吗", created: now - 180000 },
  { id: "hi", k: "steve", t: "在的。有什么需要我处理？", created: now - 170000 },
  { id: "request", k: "me", t: "帮我把对话页面整理得清晰一些，长回复也要好读。", created: now - 120000 },
  { id: "answer", k: "steve", t: "可以。我会整理消息层级、阅读间距和输入区，让每一轮对话更容易浏览。", created: now - 100000,
    steps: { tools: [], ms: 1800, route: { kind: "answer", title: "", bot: "steve", project: null, tags: [], newTags: [], why: "整理对话页的视觉层级和阅读体验", model: null, fallback: false, ms: 200 } } },
]
const long: Msg = { id: "long", k: "steve", created: now, replyTo: "request", t: "已整理好页面的视觉方案。\n\n### 这次调整\n\n- **消息层级**：正文、思考和引用分开呈现。\n- **阅读体验**：长回复保持舒适的行宽，手机上自然换行。\n- **输入区**：保留草稿、事项引用和停止生成。\n- **“执行中”没有具体活动。**看不到当前执行者。\n\n```ts\nconst conversation = { message: \"这是一段用于验证窄屏代码滚动的较长内容\", status: \"ready\" }\n```\n\n| 区域 | 调整 |\n| --- | --- |\n| 顶栏 | 固定高度 |\n| 消息 | 统一阅读栏 |\n\n下一步可以在浅色和深色主题中检查效果。" }
const steve: Bot = { id: "steve", main: true, n: "Steve", a: "", role: "你的助手", pers: "", pro: 1, h: "Codex", harness: "codex", m: "", model: null, on: true, acc: "manual", ws: "", project: null, perm: "", permMode: "auto", rule: "", prompt: null, mem: [], memCount: 0, memLoaded: true, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: [] }
const dev: Bot = { ...steve, id: "dev", main: false, n: "Steve 开发 Bot" }
st.bots = { steve, dev }
const item = (id: string, t: string, s: Item["st"]): Item => ({ id, t, b: "dev", st: s, src: ["", ""], ws: "", project: null, up: "", acc: "", goals: [], now: "定位外层进度条与 Bot 执行过程两处状态摘要的渲染代码", plan: [["定位渲染代码", 1], ["统一计时来源", 0]], rounds: [], waits: [], ss: [], tl: [], outs: [], q: [], qs: null, branch: null, cwd: null, detail: false, dependents: [], created: now, updated: now })
seedItems([item("ST-47", "思考摘要不再堆叠“没有读文件”", "done"), item("ST-48", "执行进度只显示一处，计时统一", "run")])
const url = "https://github.com/Infranite/steve/actions/runs/41851954210/job/118000000000?pr=48&check_suite_focus=true"
/* what a busy conversation carries: item cards, flow-back updates, a notice, an offer and a long unbroken link */
const rich: Msg[] = [...short,
  { id: "card", k: "steve", t: "交给开发 Bot 处理。", item: "ST-47", created: now - 90000, card: { item: "ST-47", state: "done", delivery: { text: "构建记录：" + url, at: now - 60000 }, released: { version: "v0.2.29", at: now - 50000 } } },
  { id: "upd", k: "steve", t: "ST-48 有进展。", item: "ST-48", created: now - 40000, updates: [{ kind: "reply", text: "ST-48 · Steve 开发 Bot · 定位外层进度条与 Bot 执行过程两处状态摘要的渲染代码 " + url } as any] },
  { id: "note", k: "steve", t: "ST-48 的 Release 构建失败：" + url, item: "ST-48", created: now - 30000, notice: { kind: "build", level: "error", ref: "n1" } },
  { id: "offer", k: "steve", t: "路径 `hub-rs/crates/steve-store/migrations/../../../web/src/components/ui/message-scroller.tsx` 里没有用到。", created: now - 20000, offer: { title: "把对话页里没用到的消息滚动组件和它的样式一起删掉，顺便整理滚动条", state: "open" } as any },
]
/* quoting the long reply: the quote line shows its words, not its Markdown */
/* (a quote of the message right above is not shown: one message sits between them) */
const between: Msg = { id: "between", k: "me", created: now + 500, t: "我先看看" }
const quoted: Msg = { id: "quoted", k: "me", created: now + 1000, replyTo: "long", t: "好" }
st.thread = location.search.includes("empty") ? [] : location.search.includes("long") ? [...short, long, between, quoted] : location.search.includes("rich") ? rich : short
st.notices = []
let replies = 0
window.addEventListener("fixture-reply", () => { replies++; st.thread = [...st.thread, { id: "reply-" + replies, k: "steve", created: Date.now(), t: "新到的一条回复。\n\n- 第一点\n- 第二点\n- 第三点" }]; emit() })
window.addEventListener("fixture-thinking", () => { st.thread = [...short, { k: "thinking" }]; st.thinking = true; st.phase = "answer"; emit() })
function Fixture() {
  useStore()
  const [conv, setConv] = useState(new URLSearchParams(location.search).get("conv") ?? undefined)
  useEffect(() => {
    const refresh = () => hubSubs.forEach(f => f({ type: "chan_message", id: "fixture" }))
    const switchScope = (e: Event) => {
      st.workspace = { ...st.workspace, id: (e as CustomEvent<string>).detail } as typeof st.workspace
      emit()
    }
    const switchConv = (e: Event) => setConv((e as CustomEvent<string>).detail)
    window.addEventListener("fixture-refresh", refresh)
    window.addEventListener("fixture-scope", switchScope)
    window.addEventListener("fixture-conversation", switchConv)
    return () => { window.removeEventListener("fixture-refresh", refresh); window.removeEventListener("fixture-scope", switchScope); window.removeEventListener("fixture-conversation", switchConv) }
  }, [])
  return <SidebarProvider><main style={{ height: "100dvh", width: "100%", minWidth: 0 }}><ChatView conv={conv} /></main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)

/* Authoritative chat phases for Thinking State regression. No model request is made by these events. */
window.addEventListener("fixture-thinking-stage", (event) => {
  const stage = (event as CustomEvent<string>).detail
  const t0 = Date.now() - 5000
  st.live = null; st.draft = null; st.thinking = true; st.phase = "route"
  st.thread = [...short.slice(0,3), { k: "thinking" }]
  const reasoning = Array.from({length:12}, (_,i) => `第 ${i+1} 步：核对实际模型与工具事件，保留来源、失败和已执行的步骤。`).join("\n\n")
  if (stage === "thought") st.draft = { mid:"request",text:"",thought:reasoning,t0 }
  if (stage === "research" || stage === "research-update") {
    st.phase = "research"
    st.live = { mid:"request",run:"fixture-research",startedAt:t0,tools:[{name:"Read",brief:"src/app.ts",kind:"read",ok:true},{name:"Read",brief:"src/config.ts",kind:"read",ok:true}],now:{name:"Grep",brief:"model settings",kind:"search"} }
    if (stage === "research-update") { st.live.tools.push({name:"Grep",brief:"model settings",kind:"search",ok:true}); st.live.now={name:"Bash",brief:"npm run check",kind:"exec"} }
  }
  if (stage === "reply") { st.phase="answer"; st.draft={mid:"request",thought:reasoning,text:"核对完成，正在生成回答。",t0} }
  if (["complete","failure","stopped"].includes(stage)) {
    st.thinking=false
    st.thread=[...short.slice(0,3), {id:"thinking-result",k:"steve",origin:"request",created:Date.now(),t:stage==="failure"?"模型服务暂时不可用。":"已核对。",stopped:stage==="stopped"||undefined,
      ...(stage==="stopped"?{}:{steps:{route:{kind:"research",title:"",bot:"steve",project:null,tags:[],newTags:[],why:"核对模型配置",model:"fixture-model",fallback:false,ms:200},
        answer:{thought:reasoning,model:"fixture-model",ms:5000,failed:stage==="failure"},
        research:{run:"fixture-research",model:"fixture-model",ms:28000,turns:2,tools:3,stopped:stage==="failure"?"failed":undefined},
        tools:[{name:"Read",kind:"read",args:{brief:"src/app.ts"},status:"ok",ms:20},{name:"Read",kind:"read",args:{brief:"src/config.ts"},status:"ok",ms:20},{name:"Bash",kind:"exec",args:{brief:"npm run check"},status:"failed",error:"transport",ms:500}]}}) }]
  }
  emit()
})
