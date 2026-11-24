import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { api } from "../../src/api"
import i18n from "../../src/i18n"
import { st, orphans, useStore, hubSubs, type Wait } from "../../src/store"
import { readViews } from "../../src/data/sync"
import { DecisionCard } from "../../src/thread/decision-card"
import { decisionFixture, seedDecisions } from "./view-data"
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
  : { type: "ask", subjectId: "cm_in", q: "「运维群」里 李雷 找你：李雷问周四下午能不能一起过一下方案\n为什么要你：要答应或安排事情（时间、花钱、删除、隐私或人际关系），得你自己定（@了你）\n最近的对话：\n- 我：方案我晚点发\n→ 李雷：周四下午能过一下吗", draft: q.includes("nodraft") ? "" : "周四可以", since: "刚刚", decision: "dec_h", subject: "chan_message", session: null, options: q.includes("nodraft") ? ["instruct", "verbatim", "to_item", "done"] : ["send_draft", "instruct", "verbatim", "to_item", "done"], by: "steve" }
st.ready = true
if (recovery) { w.options = ["retry", "rewrite", "discard"]; w.recovery = recovery as Wait["recovery"] }
seedDecisions([w])
const calls: unknown[] = (fixture.__calls = [])
let resolved = false
let rev = 10
const refresh = () => readViews("decision", [...new Set([w.decision, fixture.__decisionId ?? w.decision])]).catch(() => {})
api.decide = async (id, choice, text) => {
  calls.push({ id, choice, text })
  if (q.includes("hold")) await new Promise<void>((r) => { fixture.__release = r })
  if (fixture.__decisionError) throw new Error("没有完成修改，请重试")
  resolved = true
  await refresh()
  return { id, status: "resolved", result: { choice } } as any
}
api.views = async (_kind, ids) => {
  if (fixture.__stateError) throw new Error("fixture state unavailable")
  const id = fixture.__decisionId ?? w.decision
  const views = ids.includes(id) && (!resolved || fixture.__stateStale)
    ? [decisionFixture({ ...w, decision: id, recovery: fixture.__recovery ?? w.recovery })] : []
  return { rev: ++rev, views, missing: ids.filter((id) => !views.some((view) => view.id === id)) }
}
fixture.__reload = () => hubSubs.forEach((f) => f({ type: "chan_message", id: "cm_out", space_id: "sp1" }))
fixture.__refresh = refresh
fixture.__preview = (question: string, draft: string) => { w.q = question; w.draft = draft; return refresh() }
function App() { useStore(); const current = orphans()[0]; return <main className="mx-auto max-w-[640px] px-4 py-6">{current ? <DecisionCard i={current.i} showItem={false} /> : <p data-closed>这条处理已关闭</p>}<Toaster /></main> }
createRoot(document.getElementById("root")!).render(<App />)
