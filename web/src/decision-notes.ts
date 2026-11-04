/* ── an approval's note in the item's conversation ──
 * the Hub writes the note into the conversation line with the choice ("不发布 steve/st-27：先别发", "拒绝：推送。原因：太早"),
 * the same words the agent was sent. Lines written before that carry only the choice; the note is still in the decision's
 * result, so it is read from there: the release or approve decision this line answered, resolved together with it. */
type Line = { ts: number; kind: string; text: string }
type Decision = { kind: string; subject_type: string; subject_id: string; question: string; resolved: number | null; result: any }

/** a line and the decision behind it are written in the same call; anything further apart is another decision */
const SAME_CALL_MS = 5000

export function withDecisionNotes<T extends Line>(timeline: T[], decisions: Decision[]): T[] {
  const answered = decisions.filter((d) => d.kind === "approve" && d.subject_type !== "tool_call" && d.resolved != null && typeof d.result?.text === "string" && d.result.text.trim())
  if (!answered.length) return timeline
  return timeline.map((t) => {
    if (t.kind !== "me") return t
    const near = (d: Decision) => Math.abs(d.resolved! - t.ts) < SAME_CALL_MS
    const rel = /^不发布 (\S+)$/.exec(t.text)
    if (rel) {
      const d = answered.find((d) => d.subject_type === "release" && d.result.approved === false && d.subject_id === rel[1] && near(d))
      return d ? { ...t, text: `${t.text}：${d.result.text.trim()}` } : t
    }
    const ap = /^(批准|拒绝)：(.*)$/s.exec(t.text)
    if (ap) {
      const yes = ap[1] === "批准"
      const d = answered.find((d) => d.subject_type !== "release" && d.result.approved === yes && d.question === ap[2] && near(d))
      return d ? { ...t, text: `${t.text}${yes ? "。补充：" : "。原因："}${d.result.text.trim()}` } : t
    }
    return t
  })
}

/** what of a conversation line the agent's prompt contains, to find the turn that answered it; "" when it was not sent */
export function promptNeedle(text: string): string {
  const rel = /^不发布 \S+：(.*)$/s.exec(text)
  if (rel) return rel[1].trim()
  if (/^(添加标签|移除标签|稍后提醒|暂停|放弃|归档|恢复|撤销了决定|同意开始|不需要|批准发布|不发布)/.test(text)) return ""
  return text.replace(/^(回答|返工|批准|拒绝)：/, "").trim()
}
