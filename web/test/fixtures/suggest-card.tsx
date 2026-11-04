import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SuggestCard } from "../../src/thread/parts"
import { Markdown } from "../../src/components/markdown"
import { msgOf, useStore } from "../../src/store"

document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const now = Date.now()
/* ST-44: the statistics request of 2026-11-05, as the hub now writes the card about an existing item */
const q = "这条也是统计页的展示要求，和 ST-40 改的是同一个页面，可以和它一起实现和验收。建议追加到 ST-40「统计页优化」，你想追加进去，还是单独建一个事项？"
const append = msgOf({ id: "append", author: "steve", text: q, created: now, meta: { suggest: { question: q, state: "open", choices: [
  { id: "item", label: "追加到 ST-40", item: "ST-40", recommended: true },
  { id: "new", label: "单独建项", title: "今日统计按小时展示", goal: "统计维度选今天时按小时汇总", project: null, bot: "dev", tags: [] },
] } } } as any)
/* an older card: answer here or make an item */
const old = msgOf({ id: "old", author: "steve", text: "这件事要我先在这里出个方案，还是直接建成事项交给 Bot 去做？", created: now, meta: { suggest: { question: "", state: "open", choices: [
  { id: "here", label: "在这里出方案" }, { id: "item", label: "建成事项：统计页优化", title: "统计页优化" },
] } } } as any)
function Fixture() {
  useStore()
  return <main style={{ maxWidth: 760, margin: "auto", padding: 20 }} className="flex flex-col gap-8">
    {[append, old].map((m) => <div key={m.id}><Markdown text={m.t ?? ""} /><SuggestCard m={m} /></div>)}
  </main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
