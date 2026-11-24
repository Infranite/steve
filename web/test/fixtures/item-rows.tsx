import { seedItems } from "./view-data"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ItemRow } from "../../src/view-shared"
import { ItemsView } from "../../src/items-list"
import { SidebarInset, SidebarProvider } from "../../src/components/ui/sidebar"
import { st, useStore, type Item, type Bot, type Wait } from "../../src/store"

document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const dev: Bot = { id: "dev", n: "Steve 开发", a: "", role: "", pers: "", pro: 1, h: "Codex", harness: "codex", m: "", model: null, on: true, acc: "manual", ws: "", project: null, perm: "", permMode: "auto", rule: "", prompt: null, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: [] }
const item = (id: string, t: string, x: Partial<Item>): Item => ({ id, t, b: "dev", st: "run", phase: "active", src: ["", ""], ws: "", project: null, up: "02:03", acc: "", goals: [], progress: "", rounds: [], waits: [], ss: [], tl: [], outs: [], anns: [], q: [], qs: null, branch: null, cwd: null, detail: false, dependents: [], created: now, updated: now, ...x })
const wait = (type: Wait["type"], q: string, subject = "action"): Wait => ({ type, q, since: "3 分钟", decision: "d-" + q, subject, session: null, options: ["yes", "no"], by: "dev" })
const approve = wait("approve", "合并 PR #12 并打 tag 发版")
st.bots = { dev }
const rows = [
  /* a reopened cycle nobody has spoken to yet: it waits for your word, the summary says so */
  item("ST-49", "对话页左右滑动白线显示", { st: "wait", progress: "新周期，等你说要做什么" }),
  /* the same cycle once the bot took it up: running, its own plan */
  item("ST-48", "修复执行进度重复展示与计时不一致", { st: "run", progress: "新周期，收到新消息，开始处理", plan: [["查构建日志", 0]] }),
  /* a real wait: the card's question and the way to it, instead of the stale summary */
  item("ST-52", "发布脚本加上回滚", { st: "wait", progress: "交付了，等你验收", plan: [["改脚本", 1], ["测试", 1]], wait: approve, waits: [approve, wait("ask", "回滚保留几个版本？")] }),
  item("ST-53", "选择数据库迁移方案", { st: "wait", progress: "等你回答", wait: wait("ask", "迁移要在白天还是晚上做？这会影响要不要先停写入，一个很长的问题，看看会不会把行撑出去"), waits: [] }),
  item("ST-5", "修复权限状态", { progress: "正在检查边界" }),
  item("ST-10", "整理发布说明", { st: "pause", progress: "已暂停 · 等下周发版" }),
  item("ST-44", "下一周期继续", { cycle: 2, progress: "正在补齐测试", q: [{ id: "q1", text: "补充一", via: "me", created: now }, { id: "q2", text: "补充二", via: "me", created: now }], ss: ["s1", "s2"], parent: { item: "ST-1" } }),
  item("ST-27", "另一项", { progress: "正在处理" }),
  item("ST-28", "普通派生", { parent: { item: "ST-27" }, progress: "独立工作" }),
  /* closed as failed: a finished row in its own group, not lost from the list */
  item("ST-43", "独立审查回归覆盖", { st: "fail", phase: "closed", progress: "失败：评审不通过" }),
  item("ST-30", "已完成的独立事项", { st: "done", phase: "closed", progress: "已完成归档前展示" }),
]
rows[3].waits = [rows[3].wait!]
seedItems(rows)
function Fixture() {
  useStore()
  return <main style={{ maxWidth: 880, margin: "auto", padding: 20 }} className="flex flex-col">{rows.map((i) => <ItemRow key={i.id} i={i} />)}</main>
}
createRoot(document.getElementById("root")!).render(new URLSearchParams(location.search).has("groups") ? <SidebarProvider><SidebarInset className="relative min-w-0 overflow-hidden"><ItemsView /></SidebarInset></SidebarProvider> : <Fixture />)
