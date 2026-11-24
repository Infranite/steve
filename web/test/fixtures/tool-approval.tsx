import { fixtureDecisionAnswer, fixtureItem, fixtureItemDetail, fixtureWait, seedItems } from "./view-data"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { api } from "../../src/api"
import { st, useStore, type Item, type Wait } from "../../src/store"
import { DecisionCard } from "../../src/thread/decision-card"
/* a tool permission (Approval Card) with allow once and allow always; `?approve`: a plain approval (Decision Card) */
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const approve = location.search.includes("approve")
const w: Wait = approve
  ? fixtureWait({ q: "要把这条消息发出去吗？", draft: "", decision: "dec_2", subject: "item", session: "ses_1", options: ["yes", "no"] })
  : fixtureWait({ q: "Bash: npm test", draft: '{"command":"npm test"}', decision: "dec_1", subject: "tool_call", session: "ses_1", options: ["opt_once|Allow once|allow_once", "opt_always|Always allow|allow_always", "opt_reject|Reject|reject_once"] })
const i: Item = fixtureItem({ id: "ST-8", t: "跑测试", st: "wait", project: "", progress: "等你批准", wait: w, waits: [w], branch: "", cwd: "/x" })
st.ready = true
seedItems([i])
const calls: unknown[] = (window.__calls = [])
api.decide = async (id, choice, text) => { calls.push({ id, choice, text }); return fixtureDecisionAnswer({ id, kind: w.type, subject_type: w.subject, result: { choice } }) }
api.item = async () => fixtureItemDetail({ id: i.id })
function App() { useStore(); return <main className="mx-auto max-w-[640px] px-4 py-6"><DecisionCard i={i} /><Toaster /></main> }
createRoot(document.getElementById("root")!).render(<App />)
