import { createRoot } from "react-dom/client"
import "../../src/index.css"
import type { ConvMessage, Reaction } from "../../src/api/conversations"
import { ChanOutNote, ReactionChip } from "../../src/thread/conversations"

const refusal = "ext_ws target_forbidden (-32016)"
const reaction = (state: Reaction["state"], error: string | null): Reaction => ({ state, error, emoji: null, as_user: false, updated: 1 })
const note = (status: string, reason?: string) => {
  const m: ConvMessage = { id: status, author: "steve", text: "", item_id: null, created: 0, meta: { chan_out: { status, reason } } }
  return <ChanOutNote m={m} />
}

createRoot(document.getElementById("root")!).render(<main className="mx-auto max-w-xl space-y-4 p-4">
  <p data-summary>只读到了部分片段，无法覆盖最近两天。</p>
  <div data-case="eligibility">{note("skipped", "未核实为本回合经渠道生成器确认的正式回复")}</div>
  <div data-case="blocked">{note("blocked", refusal)}</div>
  <div data-case="draft">{note("draft")}</div>
  <div data-case="unknown">{note("unknown")}</div>
  <div data-case="sent">{note("sent")}</div>
  <div data-case="silent-skip">{note("skipped")}</div>
  <div data-case="reaction-cleared"><ReactionChip r={reaction("cleared", refusal)} /></div>
  <div data-case="reaction-done-rejected"><ReactionChip r={reaction("done", refusal)} /></div>
  <div data-case="reaction-accepted"><ReactionChip r={reaction("done", null)} /></div>
</main>)
