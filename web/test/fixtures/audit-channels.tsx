import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { DecisionCard } from "../../src/ui"
import { AccountView } from "../../src/channels/account"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, useStore, type Item, type Wait } from "../../src/store"

/* `?account`: a bot account's page (its undo window); otherwise a flagged channel draft (“改写后再看”) and an
 * unchecked mail draft (“检查披露”), from the mocked API */
document.documentElement.classList.toggle("dark", location.search.includes("dark"))
const now = Date.now()
const orphan = (w: Wait): Item => ({ id: "", t: "", b: "steve", st: "wait", src: ["", ""], ws: "", project: null, up: "", acc: "", goals: [], now: "", rounds: [], wait: w, waits: [w], ss: [], tl: [], outs: [], anns: [], q: [], qs: null, branch: null, cwd: null, detail: false, dependents: [], created: now, updated: now })
const chan: Wait = { type: "approve", subject: "chan_message", subjectId: "cm_1", q: "以值班小运发到「运维群」（可能泄露：「30 万」）", draft: "Acme 的预算有 30 万。", since: "刚刚", decision: "dc_chan", session: null, options: ["yes", "no", "rewrite"], by: "steve" }
const plain: Wait = { ...chan, decision: "dc_plain", subjectId: "cm_2", q: "以你本人发到「Steve 开发群」", draft: "周四可以。", options: ["yes", "no"] }
const mail: Wait = { type: "approve", subject: "mail_draft", subjectId: "md_1", q: "发邮件给 Alice：Re: 预算", draft: "", since: "刚刚", decision: "dc_mail", session: null, options: ["send", "no"], by: "steve" }
const cards = [chan, plain, mail].map((w) => ({ i: orphan(w), w }))
st.orphans = cards as typeof st.orphans
function Fixture() {
  useStore()
  if (location.search.includes("account")) return <SidebarProvider><main style={{ width: "100%", minWidth: 0 }}><AccountView id="conn_bot" /></main></SidebarProvider>
  return <main style={{ maxWidth: 640, margin: "auto", padding: 20 }} className="flex flex-col gap-4">{cards.map(({ i, w }) => <DecisionCard key={w.decision} i={i} w={w} showItem={false} />)}</main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
