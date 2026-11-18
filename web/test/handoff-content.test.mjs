import assert from "node:assert/strict"
import { test } from "node:test"
import { handoffContent } from "../src/channels/handoff-content.ts"

test("the Hub's handoff separates the original speaker, words, summary and context", () => {
  assert.deepEqual(handoffContent("「项目群」里 陈宁 找你：陈宁约你明天下午对齐方案\n为什么要你：需要你确认时间\n最近的对话：\n- 我：方案稍后发\n- 王然：我会参加\n→ 陈宁：明天下午2点可以吗？"), {
    question: "陈宁约你明天下午对齐方案", space: "项目群", starred: false,
    sender: "陈宁", text: "明天下午2点可以吗？", context: "为什么要你：需要你确认时间",
    before: "- 我：方案稍后发\n- 王然：我会参加",
  })
})

test("preserves starred contacts, empty messages and punctuation in the name or words", () => {
  const content = handoffContent("⭐ 星标联系人 · 「项目群」里 陈：宁 找你：确认\n最近的对话：\n→ 陈：宁：正文：<script>example</script>")
  assert.equal(content.starred, true)
  assert.equal(content.sender, "陈：宁")
  assert.equal(content.text, "正文：<script>example</script>")
  assert.equal(content.before, "")
  assert.equal(handoffContent("「群」里 陈宁 找你：确认\n最近的对话：\n→ 陈宁：").text, "")
})

test("never treats an arbitrary question or incomplete excerpt as a known handoff", () => {
  for (const question of ["老板在吗", "「群」里 陈宁 找你：确认", "确认\n最近的对话：\n- 陈宁：问题", "确认\n最近的对话：\n→ 陈宁：问题\n后续未知内容", "确认\n最近的对话：\n不能识别的前文\n→ 陈宁：问题"]) {
    assert.equal(handoffContent(question), null, question)
  }
})

test("a different heading remains intact while the explicit target line supplies the speaker", () => {
  const content = handoffContent("需要你决定\n为什么：对方问时间\n最近的对话：\n→ 陈宁：周四可以吗")
  assert.equal(content.question, "需要你决定")
  assert.equal(content.sender, "陈宁")
  assert.equal(content.space, undefined)
})
