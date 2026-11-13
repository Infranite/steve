import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { api, type REvent, type RSession, type State } from "../../src/api"
import { stats } from "../../src/api/stats"
import { kb } from "../../src/api/knowledge"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useStore } from "../../src/store"
import { SessionView } from "../../src/views"

/* The session page of an item's session (ST-2, a collaboration unit of ST-1) that ran two turns on Claude Code, with
 * its context, usage, one delivered output, edited files and the knowledge it was given; ?codex: an imported Codex
 * transcript without an item, nothing reported. */
const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark"))
const codex = q.has("codex")
const T = Date.now() - 3_600_000, s = (n: number) => T + n * 1000
const sess: RSession = { id: "s1", workspace_id: "default", item_id: codex ? null : "ST-2", no: 1, bot_id: "dev", harness: codex ? "codex" : "claude-code", model: codex ? null : "claude-opus-5-5", cwd: "/tmp/st-2",
  status: "idle", used: codex ? null : 54_000, size: codex ? null : 200_000, cost: 1.2, title: "核实数据契约", device_id: "hub", source: codex ? "本机 Codex" : "Steve", readonly: codex, busy: false, queued: [], created: s(0), updated: s(400) } as unknown as RSession
const other = { ...sess, id: "s0", no: 2, title: "上一轮", status: "ended" } as RSession
let seq = 0
const ev = (n: number, kind: string, data: any): REvent => ({ seq: ++seq, ts: s(n), kind, data })
const tool = (n: number, v: any): REvent => ev(n, "tool", { tool: { id: "t" + seq, origin: "harness", phase: "succeeded", ...v } })
const edit = (n: number, path: string, add: number, del: number) => tool(n, { name: "Edit", category: "edit", title: "Edit " + path, target: path, result: [{ type: "diff", path, added: add, removed: del, old: "", new: "" }] })
const events: REvent[] = [
  ev(10, "user", { text: "核实协作区的数据契约", via: "me" }), ev(12, "text", { role: "thought", text: "先读代码。" }), edit(40, "web/src/a.ts", 12, 3), edit(60, "web/src/b.ts", 4, 0),
  ev(90, "text", { role: "assistant", text: "核实完了。" }), ev(100, "turn_end", { stopReason: "end_turn" }),
  ev(200, "user", { text: "再补一个截图", via: "me" }), edit(260, "web/src/a.ts", 2, 2), ev(300, "text", { role: "assistant", text: "补好了。" }), ev(320, "turn_end", { stopReason: "end_turn" }),
]
const item = (id: string, no: number, title: string, status: string, extra: any = {}) => ({ id, workspace_id: "default", no, kind: "task", title, status, stage: "", bot_id: "dev", project_dir: null, cwd: null, branch: null, plan: [], now: "", accept_mode: "inherit", priority: "中", archived: false, created: s(0), updated: s(400), project_id: null, star: false, due: null, snooze: null, source: ["对话", "chat"], parent: null, phase: "active", cycle: 1, revision: 1, goal: "", goals: [], tags: [], deps: [], sessionIds: [], open: [], queued: [], live: false, ...extra })
const items = codex ? [] : [item("ST-1", 1, "重设计协作区", "run"), item("ST-2", 2, "核实数据契约", "done", { parent: { item: "ST-1" }, source: ["协作工作", "workflow"], sessionIds: ["s1", "s0"] }), item("ST-3", 3, "设计三层结构", "run", { parent: { item: "ST-1" }, source: ["协作工作", "workflow"], deps: ["ST-2"] })]
const state = { version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items, decisions: [], recent: [], allBots: [],
  bots: [{ id: "dev", name: "开发", role: "", main: false, harness: "claude-code", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: codex ? [sess] : [sess, other], devices: [{ id: "hub", name: "Hub", roles: [], os: "", version: "", status: "online", max_sessions: 4, seen: 0, created: 0 }],
  runtimes: [{ id: "claude-code", name: "Claude Code" }, { id: "codex", name: "Codex" }] } as unknown as State
const usage = (input: number, output: number, cache_read: number, est: number) => ({ input, output, cache_read, cache_write: 0, total: input + output + cache_read, hit_rate: cache_read / (input + cache_read), est, reported: 0, unpriced_tokens: 0, rough: false })
api.state = async () => structuredClone(state)
api.events = async (_id: string, after = 0) => events.filter((e) => e.seq > after)
api.item = (async (id: string) => ({ ...items.find((x) => x.id === id), goalsList: [], planRounds: [], decisions: [], dependents: id === "ST-2" ? ["ST-3"] : [], annotations: [],
  timeline: [{ id: 1, ts: s(320), actor: "sys", kind: "sys", text: "交付", session_id: "s1" }, { id: 2, ts: s(500), actor: "sys", kind: "sys", text: "交付", session_id: "s0" }],
  artifacts: [{ id: "art1", kind: "报告", title: "数据契约核实摘要", location: "outputs/a.md", created: s(320) }, { id: "art2", kind: "报告", title: "别的会话的产出", location: null, created: s(500) }] })) as any
api.resumeInfo = (async () => ({ harness: sess.harness, id: null, cwd: "/tmp/st-2", cwd_exists: true, command: null, transcript: null })) as any
/* the harness's own bill differs from Steve's estimate: the panel says both */
stats.session = async () => ({ session_id: "s1", turns: codex ? 0 : 2, usage: codex ? usage(0, 0, 0, 0) : { ...usage(40_000, 5_000, 900_000, 0.42), reported: 1.2 } })
stats.item = (async () => ({ turns: [{ id: "t1", session_id: "s1", started: s(10), ended: s(100), stop_reason: "end_turn", attempt: 0, transient: false, tools: 2, tool_errors: 0, usage: usage(30_000, 3_000, 600_000, 0.3) },
  { id: "t2", session_id: "s1", started: s(200), ended: s(320), stop_reason: "end_turn", attempt: 0, transient: false, tools: 1, tool_errors: 0, usage: usage(10_000, 2_000, 300_000, 0.12) }] })) as any
kb.uses = (async () => ({ hits: codex ? [] : [
  { n: 1, type: "chunk", id: "kd_1", seq: 0, level: "conclusion", title: "设备页方案", label: "事项记录 · 设备页方案", href: "#/knowledge/doc/kd_1", deleted: false, use_id: "u1", query: "协作区 数据契约", created: s(10) },
  { n: 2, type: "chunk", id: "kd_1", seq: 3, level: "conclusion", title: "设备页方案", label: "事项记录 · 设备页方案", href: "#/knowledge/doc/kd_1", deleted: false, use_id: "u1", query: "协作区 数据契约", created: s(10) },
] })) as any
void refresh()
function Fixture() {
  useStore()
  return <TooltipProvider><SidebarProvider><div className="h-dvh w-full"><SessionView k="s1" /></div></SidebarProvider></TooltipProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
