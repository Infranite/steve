import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { api, type REvent, type RItemDetail, type Shell } from "../../src/api"
import { stats, type ItemDetail, type ItemFacts, type Usage, type UsageCoverage } from "../../src/api/stats"
import { kb, type Use } from "../../src/api/knowledge"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useSession, useStore, type Item, type Sess } from "../../src/store"
import { ItemStatsRows, StatsItemView } from "../../src/stats/drill"
import SessionView from "../../src/session/view"
import { fixtureCoverage, fixtureItemDetail, fixtureRawBot, fixtureShell, seedItems, seedSessions } from "./view-data"

/* The session page of an item's session (ST-2, a collaboration unit of ST-1) that ran two turns on Claude Code, with
 * its context, usage, one delivered output, edited files and the knowledge it was given; ?codex: an imported Codex
 * transcript without an item, nothing reported. */
const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark"))
const codex = q.has("codex")
const T = Date.now() - 3_600_000, s = (n: number) => T + n * 1000
const sess = {
  item: codex ? null : "ST-2", n: 1, b: "dev", harness: codex ? "codex" : "claude-code",
  m: codex ? undefined : "claude-opus-5-5", cwd: "/tmp/st-2", st: "idle",
  ctxUsed: codex ? undefined : 54_000, ctxSize: codex ? undefined : 200_000,
  title: "核实数据契约", device: "hub", node: "hub", src: codex ? "本机 Codex" : "Steve",
  ro: codex, since: s(0), at: s(400),
} satisfies Partial<Sess>
const other = { ...sess, n: 2, title: "上一轮", st: "ended" } satisfies Partial<Sess>
let seq = 0
const ev = (n: number, kind: string, data: Record<string, unknown>): REvent => ({ seq: ++seq, ts: s(n), kind, data })
const tool = (n: number, v: Record<string, unknown>): REvent => ev(n, "tool", { tool: { id: "t" + seq, origin: "harness", phase: "succeeded", ...v } })
const edit = (n: number, path: string, add: number, del: number) => tool(n, { name: "Edit", category: "edit", title: "Edit " + path, target: path, result: [{ type: "diff", path, added: add, removed: del, old: "", new: "" }] })
const events: REvent[] = [
  ev(10, "user", { text: "核实协作区的数据契约", via: "me" }), ev(12, "text", { role: "thought", text: "先读代码。" }), edit(40, "web/src/a.ts", 12, 3), edit(60, "web/src/b.ts", 4, 0),
  ev(90, "text", { role: "assistant", text: "核实完了。" }), ev(100, "turn_end", { stopReason: "end_turn" }),
  ev(200, "user", { text: "再补一个截图", via: "me" }), edit(260, "web/src/a.ts", 2, 2), ev(300, "text", { role: "assistant", text: "补好了。" }), ev(320, "turn_end", { stopReason: "end_turn" }),
]
type SeedItem = Pick<Item, "id" | "t" | "st" | "b"> & Partial<Item>
const item = (id: string, title: string, st: Item["st"], extra: Partial<SeedItem> = {}): SeedItem => ({
  id, t: title, st, b: "dev", src: ["对话", "chat"], created: s(0), updated: s(400), ...extra,
})
const items = codex ? [] : [
  item("ST-1", "重设计协作区", "run"),
  item("ST-2", "核实数据契约", "done", { parent: { item: "ST-1" } }),
  item("ST-3", "设计三层结构", "run", { parent: { item: "ST-1" }, deps: ["ST-2"] }),
]
const state: Shell = fixtureShell({
  bots: [fixtureRawBot({ id: "dev", name: "开发" })],
  harnessNames: { "claude-code": "Claude Code", codex: "Codex" },
})
const usage = (input: number, output: number, cache_read: number, est: number): Usage => {
  const total = input + output + cache_read
  return {
    coverage: fixtureCoverage({ total, input, output, cache_read, cache_write: 0 }),
    input, output, cache_read, cache_write: 0, total,
    hit_rate: input + cache_read ? cache_read / (input + cache_read) : null,
    est, reported: 0, unpriced_tokens: 0, rough: false,
  }
}
const emptyFacts = (turns: number): ItemFacts => ({
  created: T, first_turn: T, first_deliver: null, last_deliver: null, accepted: null, done_at: null, dropped_at: null,
  lead_ms: null, cycle_ms: null, wait_ms: null, agent_ms: 1000, turns, delivers: 0, reworks: 0, first_pass: null,
  fails: 0, stalls: 0, releases: 0, release_fails: 0, decisions: 0,
})
api.shell = async () => structuredClone(state)
api.devices = async () => ({ devices: [{ id: "hub", node_id: "hub", name: "Hub", roles: [], os: "", version: "", status: "online", max_sessions: 4, seen: 0, created: 0 }] })
api.transcript = async () => ({ events, next: null })
api.queued = async () => []
seedItems(items)
seedSessions(codex ? { s1: sess } : { s1: sess, s0: other })
api.item = async (id: string): Promise<RItemDetail> => fixtureItemDetail({
  id, dependents: id === "ST-2" ? ["ST-3"] : [], annotations: [], planRounds: [],
  timeline: [{ id: 1, ts: s(320), actor: "sys", kind: "sys", text: "交付", session_id: "s1" }, { id: 2, ts: s(500), actor: "sys", kind: "sys", text: "交付", session_id: "s0" }],
  artifacts: [{ id: "art1", kind: "报告", title: "数据契约核实摘要", location: "outputs/a.md", created: s(320), delivery_id: null }, { id: "art2", kind: "报告", title: "别的会话的产出", location: null, created: s(500), delivery_id: null }],
})
api.resumeInfo = async () => ({ harness: sess.harness, id: null, cwd: sess.cwd, cwd_exists: true, command: null, transcript: null })
const notReportedUsage = (): Usage => ({
  coverage: fixtureCoverage({ state: "not_reported", expected: 0, reported: 0, complete: 0, pending: 0, unavailable: 0, total: null, input: null, output: null, cache_read: null, cache_write: null, cost_reported: null }),
  input: 0, output: 0, cache_read: 0, cache_write: 0, total: 0, hit_rate: null, est: 0, reported: 0, unpriced_tokens: 0, rough: false,
})
/* the harness's own bill differs from Steve's estimate: the panel says both */
stats.session = async () => ({ session_id: "s1", turns: codex ? 0 : 2, usage: codex ? notReportedUsage() : { ...usage(40_000, 5_000, 900_000, 0.42), reported: 1.2 } })
stats.item = async (): Promise<ItemDetail> => ({
  item_id: "ST-2", no: 2, title: "核实数据契约", status: "done", facts: emptyFacts(2),
  usage: usage(40_000, 5_000, 900_000, 0.42), age_ms: 400_000,
  turns: [
    { id: "t1", session_id: "s1", started: s(10), ended: s(100), stop_reason: "end_turn", attempt: 0, transient: false, tools: 2, tool_errors: 0, usage: usage(30_000, 3_000, 600_000, 0.3) },
    { id: "t2", session_id: "s1", started: s(200), ended: s(320), stop_reason: "end_turn", attempt: 0, transient: false, tools: 1, tool_errors: 0, usage: usage(10_000, 2_000, 300_000, 0.12) },
  ],
})
const hits: Use[] = codex ? [] : [
  { n: 1, type: "chunk", id: "kd_1", seq: 0, level: "conclusion", title: "设备页方案", label: "事项记录 · 设备页方案", href: "#/knowledge/doc/kd_1", deleted: false, use_id: "u1", query: "协作区 数据契约", created: s(10) },
  { n: 2, type: "chunk", id: "kd_1", seq: 3, level: "conclusion", title: "设备页方案", label: "事项记录 · 设备页方案", href: "#/knowledge/doc/kd_1", deleted: false, use_id: "u1", query: "协作区 数据契约", created: s(10) },
]
kb.uses = async () => ({ hits })
// Accounting states: three existing surfaces consume exactly the same API usage shape.
const parseCoverage = (v: string | null): UsageCoverage["state"] | null =>
  v === "not_reported" || v === "unavailable" || v === "partial" || v === "complete" ? v : null
const coverageState = parseCoverage(q.get("coverage"))
if (coverageState) {
  const total = coverageState === "partial" ? 12 : coverageState === "complete" ? 0 : null
  const u: Usage = { ...usage(total ?? 0, 0, 0, 0), hit_rate: null, coverage: {
    state: coverageState, expected: 1, reported: total == null ? 0 : 1, complete: coverageState === "complete" ? 1 : 0,
    pending: coverageState === "not_reported" ? 1 : 0, unavailable: coverageState === "unavailable" ? 1 : 0,
    total, input: total, output: coverageState === "complete" ? 0 : null, cache_read: coverageState === "complete" ? 0 : null, cache_write: coverageState === "complete" ? 0 : null, cost_reported: null,
  } }
  stats.session = async () => ({ session_id: "s1", turns: 1, usage: u })
  stats.item = async (): Promise<ItemDetail> => ({ item_id: "ST-2", no: 2, title: "用量账本", status: "run", usage: u, age_ms: 1000,
    facts: emptyFacts(1), turns: [], marks: [], decisions: [], by_model: [] })
  stats.tree = async () => ({ nodes: [], truncated: false })
}
void refresh()
function Fixture() {
  useStore()
  const session = useSession("s1")
  return <TooltipProvider><SidebarProvider><div className="h-dvh w-full"><SessionView k="s1" s={session} />{coverageState && <div className="p-6"><ItemStatsRows id="ST-2" render={(u) => <p data-item-usage-text>{u}</p>} /><StatsItemView id="ST-2" /></div>}</div></SidebarProvider></TooltipProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
