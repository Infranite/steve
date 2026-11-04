import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import { api, type A2uiSubmit, type RMessage, type State } from "../../src/api"
import { ChatView } from "../../src/chat-view"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, st, emit, useStore } from "../../src/store"

/* ST-46: the conversation with tables and A2UI interfaces, fed through the store as the Hub sends it. The Hub is played
 * here and keeps what it was sent in localStorage, so a reload sees what was submitted. `?stream`: a reply still being
 * written whose ```a2ui fence is not closed yet. */
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const t0 = Date.UTC(2026, 10, 5, 1, 0)
const L = (...xs: unknown[]) => xs.map((x) => JSON.stringify(x)).join("\n")
const V = "v0.9.1"

const plan = `建议这样设计：

| 层面 | 方案 |
| --- | --- |
| Steve 托管 Git | 使用 Steve 管理的固定版本 Git 和仓库缓存，不依赖用户系统安装的 Git；按平台校验、升级并支持回退 |
| 工作区创建 | 每个仓库共享对象库，每个任务保留独立工作区；记录任务、分支、路径和占用，统一管理生命周期 |
| 构建与依赖 | 优先接入包管理器共享缓存；Rust 可评估 \`sccache\`。不要直接让所有并发任务共用一个 \`target\`，否则可能争锁、互相影响 |
| 空间控制 | 开工前检查余量，限制高消耗构建的并发；低于阈值时暂停新任务，并优先回收可重建缓存 |
| 归档回收 | 任务归档且交付已保存后，自动回收工作区和专属缓存；保留提交、日志、最终产物及恢复所需信息 |
| 清理保护 | 有运行中的进程、未保存修改或尚未交付的提交时禁止清理；清理可重试，重启后能继续 |

宽表格：

| 事项 | 状态 | 负责 | 开始 | 结束 | 耗时 | 费用 | 会话 | 备注 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| ST-38 | 完成 | 开发 Bot | 11-03 | 11-04 | 26 小时 | $4.20 | 3 | 构建缓存清理 |
| ST-40 | 完成 | 开发 Bot | 11-04 | 11-04 | 5 小时 | $2.10 | 2 | 统计页 |

长表格：

| 序号 | 内容 |
| --- | --- |
${Array.from({ length: 40 }, (_, i) => `| ${i + 1} | 第 ${i + 1} 行 |`).join("\n")}

**开源选型可以先比较 Worktrunk、git-worktree-runner 这类工具。**`

const days = Array.from({ length: 30 }, (_, i) => new Date(Date.UTC(2026, 9, 7 + i)).toISOString().slice(0, 10))
const trend = L(
  { version: V, createSurface: { surfaceId: "trend", catalogId: "steve:catalog/v1" } },
  { version: V, updateDataModel: { surfaceId: "trend", path: "/", value: { range: ["7"], rows: days.map((d, i) => ({ day: d, done: (i * 7) % 11 + 2, made: (i * 5) % 9 + 3 })) } } },
  { version: V, updateComponents: { surfaceId: "trend", components: [
    { id: "root", component: "Column", children: ["pick", "chart", "dist"] },
    { id: "pick", component: "ChoicePicker", label: "范围", displayStyle: "chips", options: [{ label: "近 7 天", value: "7" }, { label: "近 30 天", value: "30" }], value: { path: "/range" } },
    { id: "chart", component: "Chart", kind: "line", title: "新建与完成", x: "day", series: [{ key: "done", label: "完成" }, { key: "made", label: "新建" }], data: { path: "/rows" }, last: { path: "/range" }, unit: "件" },
    { id: "dist", component: "Chart", kind: "hbar", title: "按状态", x: "s", series: [{ key: "n", label: "件数" }], data: [{ s: "进行中", n: 6 }, { s: "等你", n: 3 }, { s: "受阻", n: 1 }] },
  ] } },
)
const form = L(
  { version: V, createSurface: { surfaceId: "release", catalogId: "steve:catalog/v1" } },
  { version: V, updateDataModel: { surfaceId: "release", path: "/", value: { form: { name: "", scope: ["web"], notify: true, at: "2026-11-06" } } } },
  { version: V, updateComponents: { surfaceId: "release", components: [
    { id: "root", component: "Card", child: "body" },
    { id: "body", component: "Column", children: ["title", "name", "scope", "notify", "at", "actions"] },
    { id: "title", component: "Text", variant: "h2", text: "发布前确认" },
    { id: "name", component: "TextField", label: "版本说明", value: { path: "/form/name" } },
    { id: "scope", component: "ChoicePicker", label: "范围", variant: "multipleSelection", options: [{ label: "网页", value: "web" }, { label: "Hub", value: "hub" }], value: { path: "/form/scope" } },
    { id: "notify", component: "CheckBox", label: "发布后通知我", value: { path: "/form/notify" } },
    { id: "at", component: "DateTimeInput", label: "日期", enableDate: true, value: { path: "/form/at" } },
    { id: "actions", component: "Row", children: ["ok", "no"] },
    { id: "ok", component: "Button", variant: "primary", child: "ok-t", checks: [{ condition: { call: "required", args: { value: { path: "/form/name" } } }, message: "要填版本说明" }],
      action: { event: { name: "confirm", context: { name: { path: "/form/name" }, scope: { path: "/form/scope" }, notify: { path: "/form/notify" } } } } },
    { id: "ok-t", component: "Text", text: "确认发布" },
    { id: "no", component: "Button", child: "no-t", action: { event: { name: "cancel", context: {} } } },
    { id: "no-t", component: "Text", text: "先不发" },
  ] } },
)
const pick = L(
  { version: V, createSurface: { surfaceId: "pick", catalogId: "https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json" } },
  { version: V, updateComponents: { surfaceId: "pick", components: [
    { id: "root", component: "Column", children: ["q", "plan", "go"] },
    { id: "q", component: "Text", text: "选一个方案 **继续**：" },
    { id: "plan", component: "ChoicePicker", options: [{ label: "方案 A：先修表格", value: "a" }, { label: "方案 B：先接 A2UI", value: "b" }], value: ["a"] },
    { id: "go", component: "Button", child: "go-t", action: { event: { name: "choose", context: {} } } },
    { id: "go-t", component: "Text", text: "就这样" },
  ] } },
)
const broken = "```a2ui\n{\"version\":\"v0.9.1\",\"createSurface\":{\"surfaceId\":\"x\",\n```"
const unknown = L(
  { version: V, createSurface: { surfaceId: "u", catalogId: "steve:catalog/v1" } },
  { version: V, updateComponents: { surfaceId: "u", components: [{ id: "root", component: "Column", children: ["a", "b"] }, { id: "a", component: "Fancy3D" }, { id: "b", component: "Text", text: "其他组件照常显示" }] } },
)

const msg = (id: string, author: "me" | "steve", text: string, n: number, meta: Record<string, unknown> = {}): RMessage => ({ id, author, text, item_id: null, meta, created: t0 + n * 60000 })
type Server = { thread: RMessage[]; calls: number; fail: boolean }
const KEY = "fixture.a2ui.hub"
const initial = (): Server => ({ calls: 0, fail: false, thread: [
  msg("old", "steve", "旧消息照旧显示：\n\n```json\n{\"a2ui\": false}\n```", 0),
  msg("q1", "me", "对话里的表格为啥还会被框起来？", 1),
  msg("table", "steve", plan, 2),
  msg("q2", "me", "给我看看趋势，再确认一下发布", 3),
  msg("ui", "steve", "近况如下：\n\n```a2ui\n" + trend + "\n```\n\n确认后我再发：\n\n```a2ui\n" + form + "\n```", 4),
  msg("choose", "steve", "两个方案：\n\n```a2ui\n" + pick + "\n```", 5),
  msg("bad", "steve", "这条的界面写坏了，文字还在。\n\n" + broken + "\n\n不认识的组件：\n\n```a2ui\n" + unknown + "\n```", 6),
] })
const load = (): Server => { try { return JSON.parse(localStorage.getItem(KEY)!) ?? initial() } catch { return initial() } }
const save = (s: Server) => localStorage.setItem(KEY, JSON.stringify(s))
if (location.search.includes("reset")) localStorage.clear()
const hub = load(); save(hub)
Object.assign(window, { fixtureHub: { load, save } })

const state = () => ({ version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items: [], decisions: [], recent: [], allBots: [],
  bots: [{ id: "steve", name: "Steve", role: "你的助手", main: true, harness: "claude-code", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [], tags: [], tagUses: {}, thread: load().thread, settings: {}, sessions: [], devices: [], runtimes: [] }) as unknown as State
api.state = async () => state()
api.a2ui = async (id: string, a: A2uiSubmit) => {
  const s = load(); s.calls++; save(s)
  await new Promise((r) => setTimeout(r, 300))
  if (s.fail) throw new Error("Hub 暂时连不上")
  const m = s.thread.find((x) => x.id === id)!, key = `${a.block}:${a.surfaceId}`
  m.meta.a2ui ??= {}
  const had = (m.meta.a2ui as Record<string, any>)[key]
  if (had) { save(s); return { already: true, message_id: had.message, action: had } }
  const mid = `act${s.calls}`, rec = { name: a.name, label: a.label, sourceComponentId: a.sourceComponentId, context: a.context, data: a.data, at: Date.now(), message: mid }
  ;(m.meta.a2ui as Record<string, any>)[key] = rec
  s.thread.push(msg(mid, "me", `[A2UI action] 提交「${a.label}」\n${JSON.stringify({ version: V, action: a })}`, 10 + s.calls, { a2ui_action: { ...rec, data: undefined, ref: id } }))
  save(s)
  return { already: false, message_id: mid, action: rec }
}
await refresh()
if (location.search.includes("stream")) {
  st.thread = [...st.thread, { k: "thinking" }]; st.thinking = true; st.phase = "answer"
  st.draft = { mid: "q2", thought: "", text: "我画个图：\n\n```a2ui\n" + trend.split("\n").slice(0, 2).join("\n") }
  emit()
}
function Fixture() { useStore(); return <TooltipProvider><SidebarProvider><main style={{ height: "100dvh", width: "100%", minWidth: 0 }}><ChatView /></main><Toaster /></SidebarProvider></TooltipProvider> }
createRoot(document.getElementById("root")!).render(<Fixture />)
