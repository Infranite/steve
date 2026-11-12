import assert from "node:assert/strict"
import test from "node:test"
import { questionLead } from "../src/yolo/question.ts"

const cmd = 'ls /srv/app/node_modules 2>/dev/null | grep -i -e remend -e streamdown; ls /srv/app/web/node_modules 2>/dev/null | head'

test("a permission question that repeats the command keeps only what it asks", () => {
  assert.equal(questionLead("允许执行：" + cmd, [cmd]), "允许执行")
  assert.equal(questionLead("Allow running: " + cmd, [cmd]), "Allow running")
})

test("a question the Hub clipped still drops its copy of the command", () => {
  assert.equal(questionLead("允许执行：" + cmd.slice(0, 60), [cmd]), "允许执行")
})

test("a question without the command, or without commands, stays whole", () => {
  assert.equal(questionLead("合并 PR #12 并打 tag 发版", ["git push"]), "合并 PR #12 并打 tag 发版")
  assert.equal(questionLead("允许执行：" + cmd, []), "允许执行：" + cmd)
  assert.equal(questionLead(cmd, [cmd]), cmd, "nothing before the command: the question is the command")
})
