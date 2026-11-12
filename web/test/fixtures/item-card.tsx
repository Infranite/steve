import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ItemCard } from "../../src/thread/parts"
import { st, msgOf, useStore, type Item, type Bot } from "../../src/store"

document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const dev: Bot = { id: "dev", n: "Steve 开发", a: "", role: "", pers: "", pro: 1, h: "Codex", harness: "codex", m: "", model: null, on: true, acc: "manual", ws: "", project: null, perm: "", permMode: "auto", rule: "", prompt: null, mem: [], memCount: 0, memLoaded: true, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: [] }
const item = (id: string, t: string): Item => ({ id, t, b: "dev", st: "done", src: ["", ""], ws: "", project: null, up: "", acc: "", goals: [], now: "", rounds: [], waits: [], ss: [], tl: [], outs: [], q: [], qs: null, branch: null, cwd: null, detail: false, dependents: [], created: now, updated: now })
st.bots = { dev }
st.items = [item("ST-33", "排队中的消息支持撤回修改"), item("ST-40", "会话列表支持搜索"), item("ST-41", "发布脚本加上回滚"), { ...item("ST-43", "独立审查回归覆盖"), st: "fail", phase: "closed", now: "失败：评审不通过" }]
const failed = "ST-33 v0.2.23 的 Release 构建失败（failure）：https://github.com/Infranite/steve/actions/runs/41851954210。事项已按约定完成，构建需要你看一下。"
const delivery = { text: "PR #8 已经变基合并进 main，ST-33 的两个提交落在 main 的 d2070a4a 和 58d3eacb。按你的要求，没有打标签，没有发布，也没有升级本机。", at: now - 2000 }
/* as the hub keeps them: an older hub filed the failed build as the released version */
const cards = [
  msgOf({ id: "legacy", author: "steve", text: "", created: now, meta: { card: { item: "ST-33", state: "release", delivery, released: { version: failed, at: now } } } } as any).card!,
  msgOf({ id: "build", author: "steve", text: "", created: now, meta: { card: { item: "ST-40", state: "build", delivery, released: { version: "v0.2.24", at: now - 1000 }, build: { text: failed.replace("ST-33", "ST-40"), at: now } } } } as any).card!,
  msgOf({ id: "released", author: "steve", text: "", created: now, meta: { card: { item: "ST-41", state: "done", released: { version: "20261102-004055-fd0fd701", at: now } } } } as any).card!,
  /* closed as failed: the card reads closed, not running */
  msgOf({ id: "failed", author: "steve", text: "", created: now, meta: { card: { item: "ST-43", state: "running" } } } as any).card!,
]
function Fixture() {
  useStore()
  return <main style={{ maxWidth: 760, margin: "auto", padding: 20 }} className="flex flex-col">{cards.map((c) => <ItemCard key={c.item} c={c} />)}</main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
