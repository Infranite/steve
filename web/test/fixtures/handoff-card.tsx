import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { api } from "../../src/api"
import i18n from "../../src/i18n"
import { st, refresh, useStore, hubSubs, type Item, type Wait } from "../../src/store"
import { DecisionCard } from "../../src/ui"
/* Mocked API fixture, not live send acceptance. `?blocked-legacy` also covers an older Hub without a decision draft. */
if (location.search.includes("en")) void i18n.changeLanguage("en")
const q = location.search
const body = "第一段：这是拟发送的完整正文。\n第二行保持换行。\n\n" + Array.from({ length: 18 }, (_, n) => `${n + 1}. 逐项审阅发送内容，修改不会立即发出。\nhttps://example.invalid/` + "long-segment".repeat(15)).join("\n\n") + "\n\n最后一行：完整正文结束。\n<script>window.__injected = true</script>"
const fixture = window as any
fixture.__body = body
const recovery = q.includes("managed") ? { kind: "missing_permission", source: "steve.connection_capability", provider: "fixture.user", scope: "connection", code: "capability_disabled", rpc_code: null, error: "本地代发能力关闭", connection_id: "conn_review", space_id: "sp1", authorization_kind: "managed", authorization_url: "/api/chan-messages/cm_out/authorization", settings_href: "#/channels/acc/conn_review", instructions: "仅可恢复这个个人连接的本地能力；创建改动后先审阅，不会发送消息。", requires_user_action: true, external_policy_known: false } : undefined
const w: Wait = q.includes("fail") || q.includes("unknown") || q.includes("blocked") || q.includes("managed")
  ? { type: "fail", subjectId: "cm_out", q: q.includes("blocked") ? "渠道服务不允许在这个会话里发送（target_forbidden, -32016）" : "发到「运维群」的消息没有发出去：超时", draft: q.includes("legacy") ? undefined : body, since: "刚刚", decision: "dec_f", subject: "chan_message", session: null, options: q.includes("unknown") ? ["sent", "retry", "discard"] : q.includes("legacy") ? ["discard"] : q.includes("blocked") ? ["retry", "rewrite", "discard"] : ["retry", "discard"], by: "steve" }
  : q.includes("approve") ? { type: "approve", subjectId: "cm_out", q: "拟发送到「运维群」", draft: "请审阅这条草稿", since: "刚刚", decision: "dec_a", subject: "chan_message", session: null, options: ["yes", "no", "rewrite"], by: "steve" }
  : { type: "ask", subjectId: "cm_in", q: "「运维群」里 李雷 找你：老板在吗", draft: "", opts: ["draft_reply", "to_item", "done"], since: "刚刚", decision: "dec_h", subject: "chan_message", session: null, options: ["draft_reply", "to_item", "done"], by: "steve" }
const i = { id: "", t: "消息", b: "steve", st: "wait", src: ["", ""], ws: "default", project: "", up: "", acc: "inherit", goals: [], now: "", rounds: [], wait: w, waits: [w], ss: [], tl: [], outs: [], anns: [], q: [], qs: null, branch: "", cwd: "/x", detail: false, dependents: [], created: 0, updated: 0 } as unknown as Item
st.ready = true
st.orphans = [{ i, w }] as typeof st.orphans
const calls: unknown[] = (fixture.__calls = [])
let resolved = false
api.decide = async (id, choice, text) => {
  calls.push({ id, choice, text })
  if (q.includes("hold")) await new Promise<void>((r) => { fixture.__release = r })
  if (fixture.__decisionError) throw new Error("没有完成修改，请重试")
  resolved = true
  return { id, status: "resolved", result: { choice } } as any
}
api.state = async () => {
  if (fixture.__stateError) throw new Error("fixture state unavailable")
  return { version: "fixture", web_version: "fixture", workspace: { id: "default", name: "默认" }, workspaces: [], items: [],
    decisions: !resolved || fixture.__stateStale ? [{ id: fixture.__decisionId ?? w.decision, kind: w.type, subject_type: w.subject, subject_id: w.subjectId, item_id: null, session_id: null, question: w.q, draft: w.draft, options: recovery ? ["retry", "rewrite", "discard"] : w.options, status: "open", result: { recovery: fixture.__recovery ?? recovery }, requested_by: "steve", created: Date.now(), undo: null }] : [],
    recent: [], bots: [], allBots: [], projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: [], devices: [], runtimes: [] } as any
}
fixture.__reload = () => hubSubs.forEach((f) => f({ type: "chan_message", id: "cm_out", space_id: "sp1" }))
fixture.__refresh = refresh
function App() { useStore(); const current = st.orphans[0]; return <main className="mx-auto max-w-[640px] px-4 py-6">{current ? <DecisionCard i={current.i} showItem={false} /> : <p data-closed>这条处理已关闭</p>}<Toaster /></main> }
createRoot(document.getElementById("root")!).render(<App />)
