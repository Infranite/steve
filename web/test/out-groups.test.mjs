import assert from "node:assert/strict"
import { test } from "node:test"
import { groupOuts, isImageOut } from "../src/out-groups.ts"

const o = (kind, title, loc, id, at, role) => [kind, title, loc, id, at, role]

test("the latest delivery's results are the outputs; records and earlier deliveries go to the work record", () => {
  const g = groupOuts([
    o("分支", "steve/st-1", null, "a1", 1000, "result"),
    o("文件", "自查截图", "/tmp/shots", "a2", 1001),
    o("提交", "abc123 第一版", null, "a3", 1002, "result"),
    o("分支", "steve/st-1", null, "b1", 90_000, "result"),
    o("提交", "def456 返工", null, "b2", 90_001, "result"),
    o("文件", "截图", "/tmp/shots2", "b3", 90_002, "record"),
  ])
  assert.deepEqual(g.final.map((x) => [x.o[3], x.v, x.vs]), [["b1", 2, 2], ["b2", 1, 1]])
  assert.deepEqual(g.records.map((r) => [r.delivery, r.at, r.outs.map((x) => [x.o[3], x.record, x.old])]), [
    [2, 90_002, [["b3", true, false]]],
    [1, 1000, [["a1", false, true], ["a2", true, false], ["a3", false, true]]],
  ])
})

test("a delivery of records only keeps the earlier results as the outputs", () => {
  const g = groupOuts([o("文档", "方案", "docs/a.md", "a1", 1000), o("文件", "运行日志", "/tmp/run.log", "b1", 60_000)])
  assert.deepEqual(g.final.map((x) => x.o[3]), ["a1"])
  assert.deepEqual(g.records.map((r) => r.outs.map((x) => x.o[3])), [["b1"]])
})

test("what the bot said wins over the title; unsaid, the title decides", () => {
  const g = groupOuts([o("文档", "日志分析报告", "docs/r.md", "a", 1), o("文档", "中间件设计", "docs/m.md", "b", 2), o("文件", "运行日志", "/tmp/run.log", "c", 3), o("文件", "自查截图", "/tmp/s", "d", 4, "result")])
  assert.deepEqual(g.final.map((x) => x.o[3]), ["a", "b", "d"])
})

test("nothing delivered, and outputs without a time, stay in order", () => {
  assert.deepEqual(groupOuts([]), { final: [], records: [] })
  const g = groupOuts([o("PR", "PR #1", "https://x/1", "a"), o("分支", "b", null, "b")])
  assert.deepEqual(g.final.map((x) => x.o[3]), ["a", "b"])
})

test("an output is a picture by its location", () => {
  assert.ok(isImageOut(o("文件", "图", "/tmp/a.PNG", "x")))
  assert.ok(!isImageOut(o("文件", "图", "/tmp/shots/", "x")))
  assert.ok(!isImageOut(o("文件", "图", null, "x")))
})
