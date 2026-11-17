import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { api } from "../../src/api"
import i18n from "../../src/i18n"
import { st, useStore, type Item, type Wait } from "../../src/store"
import { DecisionCard } from "../../src/ui"
/* a message handed over to you (the `chan_message` ask); `?fail` / `?unknown` / `?blocked`: a reply that didn't go out; `?en` */
if (location.search.includes("en")) void i18n.changeLanguage("en")
const q = location.search
const w: Wait = q.includes("fail") || q.includes("unknown") || q.includes("blocked")
  ? { type: "fail", subjectId: "cm_out", q: "发到「运维群」的消息没有发出去：超时", draft: "好的", since: "刚刚", decision: "dec_f", subject: "chan_message", session: null, options: q.includes("unknown") ? ["sent", "retry", "discard"] : q.includes("blocked") ? ["discard"] : ["retry", "discard"], by: "steve" }
  : { type: "ask", subjectId: "cm_in", q: "「运维群」里 李雷 找你：老板在吗", draft: "", opts: ["draft_reply", "to_item", "done"], since: "刚刚", decision: "dec_h", subject: "chan_message", session: null, options: ["draft_reply", "to_item", "done"], by: "steve" }
const i = { id: "", t: "消息", b: "steve", st: "wait", src: ["", ""], ws: "default", project: "", up: "", acc: "inherit", goals: [], now: "", rounds: [], wait: w, waits: [w], ss: [], tl: [], outs: [], anns: [], q: [], qs: null, branch: "", cwd: "/x", detail: false, dependents: [], created: 0, updated: 0 } as unknown as Item
st.ready = true
st.orphans = [{ i, w }] as typeof st.orphans
const calls: unknown[] = ((window as any).__calls = [])
api.decide = async (id, choice, text) => { calls.push({ id, choice, text }); return { id, status: "resolved", result: { choice } } as any }
function App() { useStore(); return <main className="mx-auto max-w-[640px] px-4 py-6"><DecisionCard i={i} showItem={false} /><Toaster /></main> }
createRoot(document.getElementById("root")!).render(<App />)
