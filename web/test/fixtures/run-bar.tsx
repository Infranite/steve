import { fixtureRawBot, fixtureShell, seedSessions } from "./view-data"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { api, type REvent, type Shell } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { refresh, useStore } from "../../src/store"
import { RunBar, Transcript } from "../../src/thread/transcript"

/* The run bar of a turn that runs a very long command, and a turn that was interrupted after more than a minute:
 * its notes, thinking and calls, among them one of Steve's own tools with what went in. */
const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark"))
const T = Date.now(), s = (n: number) => T - (300 - n) * 1000
const sess = (id: string, no: number, running: boolean) => ({ id, item_id: null, no, bot_id: "dev", harness: "claude-code", model: "claude-opus-5-5", cwd: "/tmp/st-9", status: running ? "running" : "idle",
  used: null, size: null, title: "构建", device_id: "hub", source: "item", readonly: false, busy: running, created: s(0), updated: s(200) })
const sessions = [sess("s1", 1, true), sess("s2", 2, false)]
let seq = 0
const ev = (n: number, kind: string, data: Record<string, unknown>): REvent => ({ seq: ++seq, ts: s(n), kind, data })
const tool = (n: number, v: Record<string, unknown>, phase = "succeeded"): REvent => ev(n, "tool", { tool: { id: "t" + seq, origin: "harness", phase, ...v } })
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
const state: Shell = fixtureShell({
  bots: [fixtureRawBot({ id: "dev", name: "Steve 开发 Bot" })],
})
api.shell = async () => structuredClone(state)
api.transcript = async (id) => ({ events: events[id] ?? [], next: null })
seedSessions(Object.fromEntries(sessions.map((s) => [s.id, { item: s.item_id ?? null, b: s.bot_id, n: s.no, harness: s.harness, m: s.model, title: s.title, cwd: s.cwd, st: s.status === "running" ? "run" : s.status === "ended" ? "ended" : "idle", since: s.created, at: s.updated, busy: s.busy, ctxUsed: s.used ?? undefined, ctxSize: s.size ?? undefined }])))
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
