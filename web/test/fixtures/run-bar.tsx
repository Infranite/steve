import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { api, type REvent, type RSession, type State } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useStore } from "../../src/store"
import { RunBar, Transcript } from "../../src/ui"

/* The run bar of a turn that runs a very long command, and a turn that was interrupted after more than a minute:
 * its notes, thinking and calls, among them one of Steve's own tools with what went in. */
const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark"))
const T = Date.now(), s = (n: number) => T - (300 - n) * 1000
const sess = (id: string, no: number, running: boolean): RSession => ({ id, workspace_id: "default", item_id: null, no, bot_id: "dev", harness: "claude-code", model: "claude-opus-5-5", cwd: "/tmp/st-9", status: running ? "running" : "idle",
  used: null, size: null, cost: null, title: "构建", device_id: "hub", source: "item", readonly: false, busy: running, queued: [], created: s(0), updated: s(200) } as unknown as RSession)
const sessions = [sess("s1", 1, true), sess("s2", 2, false)]
let seq = 0
const ev = (n: number, kind: string, data: any): REvent => ({ seq: ++seq, ts: s(n), kind, data })
const tool = (n: number, v: any, phase = "succeeded"): REvent => ev(n, "tool", { tool: { id: "t" + seq, origin: "harness", phase, ...v } })
const long = "cargo test --workspace --all-features --no-fail-fast -- --test-threads=1 --nocapture workflows::cycle::replans_after_an_archived_result_without_losing_the_queue"
const events: Record<string, REvent[]> = {
  s1: [ev(250, "user", { text: "跑一遍全部测试", via: "me" }), tool(260, { name: "Bash", category: "exec", title: long, target: long, action: { type: "exec", command: long } }, "running")],
  s2: [
    ev(10, "user", { text: "暂停这个工作流，先看看它卡在哪", via: "me" }),
    ev(11, "text", { role: "thought", text: "先读工作流的状态。" }),
    tool(14, { name: "mcp__steve__steve_workflow", origin: "steve", category: "steve", title: "workflow", action: { type: "tool", args: { action: "pause", reason: "先看看卡在哪", expected_revision: 37 } } }),
    ev(16, "text", { role: "assistant", text: "已暂停，接着读代码。" }),
    tool(20, { name: "Read", category: "read", title: "Read hub-rs/src/workflows.rs", target: "hub-rs/src/workflows.rs" }),
    ev(95, "turn_end", { stopReason: "cancelled" }),
  ],
}
const state = { version: "test", web_version: "test", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items: [], decisions: [], recent: [], allBots: [],
  bots: [{ id: "dev", name: "Steve 开发 Bot", role: "", main: false, harness: "claude-code", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "auto", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions, devices: [{ id: "hub", name: "Hub", roles: [], os: "", version: "", status: "online", max_sessions: 4, seen: 0, created: 0 }], runtimes: [] } as unknown as State
api.state = async () => structuredClone(state)
api.events = async (id: string, after = 0) => (events[id] ?? []).filter((e) => e.seq > after)
void refresh()
function Fixture() {
  useStore()
  return (
    <TooltipProvider><SidebarProvider>
      <main className="flex w-full max-w-3xl flex-col gap-10 p-4 sm:p-8">
        <section data-fixture="run"><RunBar k="s1" /><Transcript k="s1" /></section>
        <section data-fixture="stopped"><Transcript k="s2" /></section>
      </main>
    </SidebarProvider></TooltipProvider>
  )
}
createRoot(document.getElementById("root")!).render(<Fixture />)
