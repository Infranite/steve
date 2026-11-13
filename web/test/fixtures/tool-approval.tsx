import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { api } from "../../src/api"
import { st, useStore, type Item, type Wait } from "../../src/store"
import { DecisionCard } from "../../src/ui"
/* a tool permission (Approval Card) with allow once and allow always; `?approve`: a plain approval (Decision Card) */
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const approve = location.search.includes("approve")
const w: Wait = approve
  ? { type: "approve", q: "要把这条消息发出去吗？", draft: "", since: "刚刚", decision: "dec_2", subject: "item", session: "ses_1", options: ["yes", "no"], by: "dev" }
  : { type: "approve", q: "Bash: npm test", draft: '{"command":"npm test"}', since: "刚刚", decision: "dec_1", subject: "tool_call", session: "ses_1", options: ["opt_once|Allow once|allow_once", "opt_always|Always allow|allow_always", "opt_reject|Reject|reject_once"], by: "dev" }
const i = { id: "ST-8", t: "跑测试", b: "dev", st: "wait", src: ["", ""], ws: "default", project: "", up: "", acc: "inherit", goals: [], now: "等你批准", rounds: [], wait: w, waits: [w], ss: [], tl: [], outs: [], anns: [], q: [], qs: null, branch: "", cwd: "/x", detail: false, dependents: [], created: 0, updated: 0 } as unknown as Item
st.ready = true
st.items = [i]
const calls: unknown[] = ((window as any).__calls = [])
api.decide = async (id, choice, text) => { calls.push({ id, choice, text }); return { id, status: "resolved", result: { choice } } as any }
api.item = async () => ({ ...i, status: "run", now: "继续中" }) as any
function App() { useStore(); return <main className="mx-auto max-w-[640px] px-4 py-6"><DecisionCard i={st.items[0] ?? i} /><Toaster /></main> }
createRoot(document.getElementById("root")!).render(<App />)
