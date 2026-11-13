import assert from "node:assert/strict"
import test from "node:test"
import { lineDiff, foldedDiff, diffCounts } from "../src/lib/line-diff.ts"

test("a line diff keeps, removes and adds lines in order", () => {
  assert.deepEqual(lineDiff("a\nb\nc", "a\nx\nc"), [{ k: "same", text: "a" }, { k: "del", text: "b" }, { k: "add", text: "x" }, { k: "same", text: "c" }])
  assert.deepEqual(lineDiff("", "new"), [{ k: "add", text: "new" }])
  assert.deepEqual(lineDiff("old", ""), [{ k: "del", text: "old" }])
  assert.deepEqual(diffCounts("a\nb", "a\nb\nc\nd"), { add: 2, del: 0 })
})

test("unchanged lines far from a change fold into one mark", () => {
  const before = Array.from({ length: 10 }, (_, n) => "l" + n).join("\n")
  const after = before.replace("l5", "L5")
  const d = foldedDiff(before, after, 1)
  assert.deepEqual(d[0], { k: "fold", n: 4 })
  assert.deepEqual(d.slice(1, 5).map((l) => l.k), ["same", "del", "add", "same"])
  assert.deepEqual(d[5], { k: "fold", n: 3 })
  /* one far line is shown, not folded */
  assert.deepEqual(foldedDiff("a\nb\nc\nd", "a\nb\nc\nD", 2).map((l) => l.k), ["same", "same", "same", "del", "add"])
})
