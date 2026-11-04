import assert from "node:assert/strict"
import { test } from "node:test"
import { promptNeedle, withDecisionNotes } from "../src/decision-notes.ts"

const line = (text, ts = 1000) => ({ id: 1, ts, kind: "me", text })
const rel = (approved, text, resolved = 1000, subject_id = "steve/st-27") => ({ kind: "approve", subject_type: "release", subject_id, question: "发布？", resolved, result: { approved, text } })
const act = (approved, text, question = "推送到 main", resolved = 1000) => ({ kind: "approve", subject_type: "action", subject_id: "x", question, resolved, result: { approved, text } })

test("a line written with its note is shown as written, and the agent's prompt is found by the note", () => {
  const t = [line("不发布 steve/st-27：走 PR 合并，不用你发布")]
  assert.deepEqual(withDecisionNotes(t, [rel(false, "走 PR 合并，不用你发布")]), t)
  assert.equal(promptNeedle(t[0].text), "走 PR 合并，不用你发布")
  assert.equal(promptNeedle("拒绝：推送到 main。原因：太早"), "推送到 main。原因：太早")
})

test("an older line without its note gets it from the decision it answered", () => {
  const out = withDecisionNotes([line("不发布 steve/st-27"), line("拒绝：推送到 main"), line("批准：发邮件")], [rel(false, " 先别发 "), act(false, "太早"), act(true, "抄送我", "发邮件")])
  assert.deepEqual(out.map((t) => t.text), ["不发布 steve/st-27：先别发", "拒绝：推送到 main。原因：太早", "批准：发邮件。补充：抄送我"])
  assert.equal(promptNeedle(out[0].text), "先别发")
})

test("nothing is made up: no note, another branch, another answer or another time leaves the line alone", () => {
  const t = [line("不发布 steve/st-27"), line("拒绝：推送到 main")]
  for (const ds of [[rel(false, ""), act(false, "")], [rel(false, "x", 1000, "steve/st-28"), act(false, "x", "别的")], [rel(true, "x"), act(true, "x")], [rel(false, "x", 60_000), act(false, "x", "推送到 main", 60_000)]])
    assert.deepEqual(withDecisionNotes(t, ds).map((x) => x.text), t.map((x) => x.text))
  assert.equal(promptNeedle("不发布 steve/st-27"), "", "a plain no was never sent to the agent")
  assert.equal(promptNeedle("批准发布 steve/st-27"), "")
})
