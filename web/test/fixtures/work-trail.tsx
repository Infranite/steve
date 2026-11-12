import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { api, type REvent, type RSession, type State } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { Trail } from "../../src/components/work-trail"
import { refresh, st, useStore, type Msg } from "../../src/store"
import { Transcript } from "../../src/ui"
import { StepsBlock, openSteps } from "../../src/chat2"
import { ResearchLive } from "../../src/thread/parts"

/* The work trail in both places: an agent's run (`?done`: the turn ended) and Steve's conversation — a finished
 * research folded open, the research under way, and reasoning streaming in. */
const q = new URLSearchParams(location.search), done = q.has("done")
document.documentElement.classList.toggle("dark", q.has("dark"))
const T = Date.now(), s = (n: number) => T - (120 - n) * 1000
const sessions: RSession[] = [{ id: "s1", workspace_id: "default", item_id: null, no: 1, bot_id: "dev", harness: "claude-code", model: "claude-opus-5-5", cwd: "/tmp/st-7", status: done ? "ended" : "running", used: 36000, size: 200000, cost: null, title: "瑞典上线调研", device_id: "hub", source: "item", readonly: false, busy: !done, queued: [], created: s(0), updated: s(60) } as RSession]
let seq = 0
const ev = (n: number, kind: string, data: any): REvent => ({ seq: ++seq, ts: s(n), kind, data })
const tool = (n: number, v: any, open = false): REvent => ev(n, "tool", { tool: { id: "t" + seq, origin: "harness", phase: open ? "running" : "succeeded", ...v } })
const read = (n: number, f: string) => tool(n, { name: "Read", category: "read", title: "Read " + f, target: f })
const grep = (n: number, p: string, open = false) => tool(n, { name: "Grep", category: "search", title: "Grep " + p, target: p, action: { type: "search", query: p } }, open)
const web = (n: number, p: string) => tool(n, { name: "WebSearch", category: "fetch", title: p, action: { type: "search", query: p } })
const run = (n: number, cmd: string, desc: string, open = false) => tool(n, { name: "Bash", category: "exec", title: cmd, target: cmd, description: desc, action: { type: "exec", command: cmd }, result: [{ type: "text", text: "ok\n3 files" }] }, open)
const edit = (n: number, f: string) => tool(n, { name: "Edit", category: "edit", title: "Edit " + f, target: f, result: [{ type: "diff", path: f, old: "a\nb", new: "a\nc\nd", added: 2, removed: 1 }] })
const thought = "目标是让商店在瑞典上线。先看 Shopify 已经知道的：谁在从瑞典下单、买什么、多久回购一次。\n\n然后往外看：本地品牌怎么定价和定位，瑞典买家对配送和退货的预期，以及瑞典和挪威相比如何。\n\n1. 读瑞典订单历史\n2. 搜本地市场资料\n3. 交给品牌研究员\n4. 对比瑞典与挪威\n5. 写上线报告\n\n订单先行，它是其余一切的基线。"
const events: REvent[] = [
  ev(1, "user", { text: "让我们在瑞典上线", via: "me" }),
  ev(2, "text", { role: "thought", text: thought }),
  read(8, "/repo/data/orders/sweden-2026.csv"), read(9, "/repo/data/orders/returns.csv"), read(9, "/repo/docs/shipping.md"), read(10, "/repo/data/customers.json"), read(10, "/repo/data/orders/norway-2026.csv"),
  web(12, "Swedish e-commerce delivery expectations 2026"), web(13, "acedesign.io sweden pricing"), web(14, "Klarna share Sweden checkout"),
  tool(16, { name: "Task", category: "think", title: "Brand Researcher", description: "品牌研究员：扫描瑞典竞品品牌" }),
  tool(18, { name: "mcp__steve__steve_update", origin: "steve", category: "steve", title: "update", action: { type: "tool", args: { now: "写上线报告", plan: [{ step: "读瑞典订单历史", done: true }, { step: "搜本地市场资料", done: true }, { step: "对比瑞典与挪威", done: true }, { step: "写上线报告", done: false }] } } }),
  ev(19, "text", { role: "assistant", text: "订单和市场都看完了，开始写报告。" }),
  edit(20, "/repo/reports/sweden-insight.md"), edit(22, "/repo/reports/summary.md"),
  run(24, "npm run build:report", "生成 PDF 报告"), run(26, "ls reports", "检查产物", !done),
  ...(done ? [ev(28, "text", { role: "assistant", text: "报告已就绪：`reports/sweden-insight.pdf`。" }), ev(29, "turn_end", { stopReason: "end_turn" })] : []),
]
const state = { version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items: [], decisions: [], recent: [], allBots: [],
  bots: [{ id: "dev", name: "Steve 开发 Bot", role: "", main: false, harness: "claude-code", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions, devices: [{ id: "hub", name: "Hub", roles: [], os: "", version: "", status: "online", max_sessions: 4, seen: 0, created: 0 }], runtimes: [] } as unknown as State
api.state = async () => structuredClone(state)
api.events = async (_id: string, after = 0) => events.filter((e) => e.seq > after)
const research: Msg = { id: "r1", k: "steve", t: "", created: T, steps: { ms: 41000, research: { run: "x", model: null, ms: 41000, turns: 3, tools: 7 },
  route: { kind: "research", title: "", bot: "steve", project: null, tags: [], newTags: [], why: "要先读代码再回答", model: "fast", fallback: false, ms: 900 },
  tools: [
    { name: "Read", kind: "read", args: { brief: "web/src/chat2.tsx" }, status: "ok", ms: 30 }, { name: "Read", kind: "read", args: { brief: "web/src/ui.tsx" }, status: "ok", ms: 40 }, { name: "Read", kind: "read", args: { brief: "web/src/thread/parts.tsx" }, status: "ok", ms: 20 },
    { name: "Grep", kind: "search", args: { brief: "TraceLine" }, status: "ok", ms: 80 }, { name: "Grep", kind: "search", args: { brief: "researchSummary" }, status: "ok", ms: 60 },
    { name: "Bash", kind: "exec", args: { brief: "git log --oneline -5" }, status: "failed", error: "transport", ms: 200 },
    { name: "mcp__steve__steve_knowledge_search", args: { brief: "思考过程展示" }, status: "ok", ms: 300 },
  ] } }
st.thread = [research]
st.live = { mid: "m", run: "live", t0: T - 14000, tools: [{ name: "Read", brief: "web/src/chat2.tsx", ok: true, kind: "read" }, { name: "Read", brief: "web/src/ui.tsx", ok: true, kind: "read" }, { name: "Grep", brief: "StepsBlock", ok: true, kind: "search" }], now: { name: "Grep", brief: "LiveSteps", kind: "search" } }
void refresh()
setTimeout(() => openSteps("r1"), 300)
function Fixture() {
  useStore()
  return (
    <TooltipProvider><SidebarProvider>
      <main className="grid w-full grid-cols-1 gap-10 p-8 lg:grid-cols-2">
        <section data-fixture="run"><h2 className="mb-4 type-label text-muted-foreground">运行详情</h2><Transcript k="s1" /></section>
        <section data-fixture="chat" className="flex flex-col gap-8"><h2 className="type-label text-muted-foreground">对话</h2>
          <div className="flex flex-col" data-fixture-steps><StepsBlock m={research} /></div>
          <div className="flex flex-col" data-fixture-live><ResearchLive /></div>
          <div data-fixture-think><Trail items={[{ t: "think", key: "th", text: thought.slice(0, 120), live: true, t0: T - 3000 }]} live /></div>
        </section>
      </main>
    </SidebarProvider></TooltipProvider>
  )
}
createRoot(document.getElementById("root")!).render(<Fixture />)
