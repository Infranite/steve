import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { QueuedComment, queuedComments, QueueStrip } from "../../src/queue"
import { Markdown } from "../../src/components/markdown"
import type { RQueued } from "../../src/api"
/* an item page in miniature: the timeline's comments, then the strip that sits above the composer */
const P = "补充信息："
const row = (id: string, via: string, text: string): RQueued => ({ id, via, text, created: 1, files: [] })
const tl: any[] = [
  ["me", "这里不支持markdown渲染。举一反三，都检查下", "23:30", "s1", undefined],
  ["sys", "更新了计划", "23:31"],
  ["me", "已经答复过的评论", "23:32", "s1", { s: "s1", from: 0, to: 1, text: "好", tools: 0, live: false }],
  ["me", "**第二条**排队的评论", "23:33", "s1", undefined],
]
const rows = [row("q1", "me", P + tl[0][1]), row("q2", "me", P + tl[3][1]), row("q3", "steve", "Steve 交代的补充"), row("q4", "me", P + "评论里找不到的")]
const { byComment, rest } = queuedComments(tl, "s1", rows, P)
createRoot(document.getElementById("root")!).render(<StrictMode><main style={{ maxWidth: 760, margin: "auto", padding: 20 }}>
  {tl.map((e, n) => {
    const q = byComment.get(n)
    return <div key={n} className="flex items-start gap-2 py-1.5" data-comment={n}>
      {q ? <QueuedComment sid="s1" q={q} prefix={P} note="本回合结束后发送" at={e[2]} body={(t) => <div className="type-body"><Markdown text={t} /></div>} />
        : <div className="type-body">{e[1]}</div>}
    </div>
  })}
  <QueueStrip sid="s1" rows={rest} prefix={P} note="本回合结束后发送" />
</main></StrictMode>)
