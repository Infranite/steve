import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import { api, type RProject } from "../../src/api"
import { st, useStore, type Item, type Wait } from "../../src/store"
import { DecisionCard } from "../../src/ui"
/* an acceptance card of an item in a project with a release (`?norelease`: a project without one) */
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const release = !location.search.includes("norelease")
const p = { id: "prj_1", workspace_id: "default", name: "Steve", description: "", device_id: "hub", path: "/x", kind: "git", worktree: true, default_bot: null, release: release ? { kind: "script", cmd: "/x/ops/release.sh" } : null, archived: false, created: 0, updated: 0 } as RProject
const w: Wait = { type: "accept", q: "做完了，请验收", draft: "改好了登录页\n\n检查：✓ npm test", since: "刚刚", decision: "dec_1", subject: "item", session: "ses_1", options: ["pass", "rework"], by: "dev" }
const i = { id: "ST-7", t: "改登录页", b: "dev", st: "wait", src: ["", ""], ws: "default", project: "prj_1", up: "", acc: "inherit", goals: [], now: "交付了，等你验收", rounds: [], wait: w, waits: [w], ss: [], tl: [], outs: [], anns: [], q: [], qs: null, branch: "steve/st-7", cwd: "/x/wt", detail: false, dependents: [], created: 0, updated: 0 } as unknown as Item
st.ready = true
st.projects = [p]
st.items = [i]
const calls: unknown[] = ((window as any).__calls = [])
api.decide = async (id, choice, text) => { calls.push({ id, choice, text }); return { id, status: "resolved", result: text === "release" ? { choice, release: "started" } : { choice } } as any }
/* what the item reads after the answer: releasing, or (`__done`) finished without a release */
api.item = async () => ((window as any).__done ? { ...i, status: "done", now: "完成" } : { ...i, status: "wait", now: "正在发布" }) as any
function App() { useStore(); return <main className="mx-auto max-w-[640px] px-4 py-6"><DecisionCard i={st.items[0] ?? i} /><Toaster /></main> }
createRoot(document.getElementById("root")!).render(<App />)
