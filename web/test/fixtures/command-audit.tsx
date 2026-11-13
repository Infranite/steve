/* ⌘K (the affectUI Command Menu over Steve's data) and 设置 · 操作记录 (the affectUI Activity list over the audit
 * trail), with a seeded store; the Hub's searches and the audit trail come from the test's routes. */
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { emit, st, useStore, type Item, type Sess } from "../../src/store"
import { onJump } from "../../src/thread/history"
import { getSheet, openSheet } from "../../src/ui"
import { CommandPalette } from "../../src/command-palette"
import { AuditSettingsPane } from "../../src/audit-settings"

document.documentElement.classList.toggle("dark", location.search.includes("dark"))
const item = (id: string, t: string, s: Item["st"], updated: number) => ({ id, t, b: "dev", st: s, src: ["", ""], ws: "default", project: null, up: "", acc: "inherit", goals: [], now: "", rounds: [], waits: [], ss: [], tl: [], outs: [], anns: [], q: [], qs: null, branch: null, cwd: null, detail: false, dependents: [], created: 0, updated }) as unknown as Item
const sess = (id: string, n: number, title: string, itemId: string | null, at: number) => ({ id, n, item: itemId, b: "dev", st: "idle", title, dur: "", ctx: 0, src: "", up: "", at, ev: [], seqs: [], ts: [], queue: [], device: "hub", loaded: false, busy: false }) as unknown as Sess
st.ready = true
st.owner = true
st.items = [item("ST-7", "改登录页", "run", 1), item("ST-8", "修复协作卡片误报", "done", 2), item("ST-9", "整理发布说明", "wait", 3)]
st.S = { ses_a: sess("ses_a", 1, "排查登录跳转", "ST-7", 2), ses_b: sess("ses_b", 2, "写测试", "ST-9", 1) }
st.bots = { steve: { n: "Steve", role: "主助手", main: true } as any, dev: { n: "开发", role: "写代码" } as any }

function App() {
  useStore()
  return location.search.includes("audit")
    ? <main className="mx-auto max-w-[760px] px-4 py-6"><AuditSettingsPane /></main>
    : <main className="p-6"><p data-route>{location.hash || "#/"}</p><button type="button" data-open onClick={() => openSheet("command")}>open</button><CommandPalette open={getSheet()?.kind === "command"} /></main>
}
/* what the test does to the store and the conversation: a session renamed in place as a stream does it, and a chat
 * view that starts listening for jumps only now */
Object.assign(window, {
  renameSession: (id: string, title: string) => { st.S[id].title = title; emit() },
  listenJump: () => new Promise((res) => { const off = onJump((j) => { off(); res(j) }); setTimeout(() => { off(); res(null) }, 1000) }),
})
window.addEventListener("hashchange", () => { const r = document.querySelector("[data-route]"); if (r) r.textContent = location.hash })
createRoot(document.getElementById("root")!).render(<App />)
