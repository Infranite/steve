import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { DecisionCard } from "../../src/ui"
import { Toaster } from "../../src/components/ui/toaster"
import { ComposerDecisions } from "../../src/thread/decide"
import { emit, refresh, st, useStore, type Item, type Wait } from "../../src/store"
import { api } from "../../src/api"

/* Local browser fixtures only. All API requests are intercepted by channel-reply.browser.mjs. */
const params = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", params.has("dark"))
const kind = params.get("case") ?? "draft"
const approval: Wait = { type: "approve", subject: "chan_message", subjectId: "cm_reply", q: "以值班助手回复「产品讨论群」的消息", draft: "我是值班助手，是负责答疑的 AI 助手。\n你可以直接告诉我需要什么帮助。", since: "刚刚", decision: "dc_reply", session: null, options: ["yes", "no", "to_item", "rewrite"], by: "steve" }
const scenario: Wait = kind === "handoff"
  ? { ...approval, type: "ask", subjectId: "cm_in", q: "陈同学找你：你是谁啊？这件事需要主人决定。", draft: "", options: ["instruct", "verbatim", "to_item", "done"] }
  : kind === "restricted" ? { ...approval, options: ["no", "to_item"] }
  : kind === "empty" ? { ...approval, draft: "", options: ["yes", "no"] }
  : kind === "plain" ? { ...approval, subject: "external_access", options: ["yes", "no"] }
  : kind === "tool" ? { ...approval, subject: "tool_call", options: ["once|Run once|allow_once", "always|Always|allow_always", "deny"], q: "Allow running: pwd", draft: "{\"command\":\"pwd\"}" }
  : kind === "failed" ? { ...approval, type: "fail", q: "消息发送失败：渠道暂时不可用", options: ["retry", "discard"] }
  : approval
const orphan = (w: Wait): Item => ({ id: "", t: "", b: "steve", st: "wait", src: ["", ""], ws: "", project: null, up: "", acc: "", goals: [], now: "", rounds: [], wait: w, waits: [w], ss: [], tl: [], outs: [], anns: [], q: [], qs: null, branch: null, cwd: null, detail: false, dependents: [], created: 1, updated: 1 })
const waits = kind === "composer" ? [scenario, { ...approval, decision: "dc_next", subjectId: "cm_next", q: "以你本人回复「另一个聊天」" }] : [scenario]
st.orphans = kind === "live" ? [] : waits.map((w) => ({ i: orphan(w), w })) as typeof st.orphans
if (kind === "live") void refresh()
function Fixture() {
  useStore()
  return <main className="mx-auto flex w-full max-w-2xl flex-col gap-4 p-4">
    <div className="type-footnote text-muted-foreground">Channel reply browser fixture · intercepted API</div>
    {kind === "composer" ? <ComposerDecisions /> : st.orphans.map(({ i, w }) => <DecisionCard key={w.decision} i={i} w={w} showItem={false} />)}
    {kind === "duplicate" && st.orphans[0] && <DecisionCard i={st.orphans[0].i} w={st.orphans[0].w} showItem={false} />}
    <button type="button" data-fixture-rebuild onClick={() => { st.orphans = [{ i: orphan({ ...scenario }), w: { ...scenario } }] as typeof st.orphans; emit() }}>Rebuild existing snapshot</button>
    <button type="button" data-fixture-fresh onClick={() => void refresh()}>Refresh open snapshot (undo)</button>
    <button type="button" data-fixture-undo onClick={() => void api.undo("dc_reply").then(() => refresh())}>Undo through fixture API</button>
    <Toaster />
  </main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
