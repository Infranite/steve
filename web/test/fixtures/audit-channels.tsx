import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { DecisionCard } from "../../src/ui"
import { AccountView } from "../../src/channels/account"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { orphans, useStore, type Wait } from "../../src/store"
import { seedDecisions } from "./view-data"

/* `?account`: a bot account's page (its undo window); otherwise a flagged channel draft (“改写后再看”) and an
 * unchecked mail draft (“检查披露”), from the mocked API */
document.documentElement.classList.toggle("dark", location.search.includes("dark"))
const chan: Wait = { type: "approve", subject: "chan_message", subjectId: "cm_1", q: "以值班小运发到「运维群」（可能泄露：「30 万」）", draft: "Acme 的预算有 30 万。", since: "刚刚", decision: "dc_chan", session: null, options: ["yes", "no", "rewrite"], by: "steve" }
const plain: Wait = { ...chan, decision: "dc_plain", subjectId: "cm_2", q: "以你本人发到「Steve 开发群」", draft: "周四可以。", options: ["yes", "no"] }
const mail: Wait = { type: "approve", subject: "mail_draft", subjectId: "md_1", q: "发邮件给 Alice：Re: 预算", draft: "", since: "刚刚", decision: "dc_mail", session: null, options: ["send", "no"], by: "steve" }
seedDecisions([chan, plain, mail])
function Fixture() {
  useStore()
  if (location.search.includes("account")) return <SidebarProvider><main style={{ width: "100%", minWidth: 0 }}><AccountView id="conn_bot" /></main></SidebarProvider>
  return <main style={{ maxWidth: 640, margin: "auto", padding: 20 }} className="flex flex-col gap-4">{orphans().map(({ i, w }) => <DecisionCard key={w.decision} i={i} w={w} showItem={false} />)}</main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
