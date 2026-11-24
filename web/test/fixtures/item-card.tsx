import { fixtureItem, seedItems } from "./view-data"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ItemCard } from "../../src/thread/parts"
import { st, msgOf, useStore, type Item, type Bot } from "../../src/store"

document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
const dev: Bot = { id: "dev", n: "Steve 开发", a: "", role: "", pers: "", pro: 1, h: "Codex", harness: "codex", m: "", model: null, on: true, acc: "manual", ws: "", project: null, perm: "", permMode: "auto", rule: "", prompt: null, routines: [], hue: "", color: "#8b9eac", round: 60, skin: null, workspaces: [] }
const item = (id: string, t: string): Item => fixtureItem({ id, t, st: "done", ws: "", acc: "", created: now, updated: now })
st.bots = { dev }
seedItems([item("ST-33", "排队中的消息支持撤回修改"), item("ST-40", "会话列表支持搜索"),
  { ...item("ST-41", "发布脚本加上回滚"), outs: [["file", "自查日志", null, "check-log", now, "record"]] },
  { ...item("ST-43", "独立审查回归覆盖"), st: "fail", phase: "closed", progress: "失败：评审不通过" }])
const failed = "ST-33 v0.2.23 的 Release 构建失败（failure）：https://github.com/Infranite/steve/actions/runs/41851954210。事项已按约定完成，构建需要你看一下。"
const delivery = { text: "PR #8 已经变基合并进 main，ST-33 的两个提交落在 main 的 d2070a4a 和 58d3eacb。按你的要求，没有打标签，没有发布，也没有升级本机。", at: now - 2000 }
const cards = [
  msgOf({ id: "failed-build", author: "steve", text: "", item_id: null, created: now, meta: { card: { item: "ST-33", state: "release", delivery, build: { text: failed, at: now } } } }).card!,
  msgOf({ id: "build", author: "steve", text: "", item_id: null, created: now, meta: { card: { item: "ST-40", state: "build", delivery, released: { version: "v0.2.24", at: now - 1000 }, build: { text: failed.replace("ST-33", "ST-40"), at: now } } } }).card!,
  msgOf({ id: "released", author: "steve", text: "", item_id: null, created: now, meta: { card: { item: "ST-41", state: "done", delivery: { text: "已交付。", at: now - 2000 }, released: { version: "20261102-004055-fd0fd701", at: now } } } }).card!,
  /* closed as failed: the card reads closed, not running */
  msgOf({ id: "failed", author: "steve", text: "", item_id: null, created: now, meta: { card: { item: "ST-43", state: "running" } } }).card!,
]
function Fixture() {
  useStore()
  return <main style={{ maxWidth: 760, margin: "auto", padding: 20 }} className="flex flex-col">{cards.map((c) => <ItemCard key={c.item} c={c} />)}</main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
