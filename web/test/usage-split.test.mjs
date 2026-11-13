import assert from "node:assert/strict"
import test from "node:test"
import { splitParts } from "../src/stats/split.ts"

const COLORS = ["c1", "c2", "c3", "c4", "c5"]
const other = { color: "grey", label: (n) => `others ${n}` }
const row = (key, est, total = 100) => ({ key, label: key.toUpperCase(), est, total })

test("six parts: four named and one 'others', so no two parts share a colour", () => {
  const { cost, items } = splitParts([row("a", 6), row("b", 5), row("c", 4), row("d", 3), row("e", 2), row("f", 1)], COLORS, other, "?")
  assert.equal(cost, true)
  assert.deepEqual(items.map((x) => [x.id, x.value, x.color]), [["a", 6, "c1"], ["b", 5, "c2"], ["c", 4, "c3"], ["d", 3, "c4"], ["other", 3, "grey"]])
  assert.equal(items.at(-1).label, "others 2")
  assert.equal(new Set(items.map((x) => x.color)).size, items.length)
})

test("five parts are all named; zeros are left out; nothing priced splits by tokens", () => {
  assert.deepEqual(splitParts([row("a", 5), row("b", 4), row("c", 3), row("d", 2), row("e", 1), row("z", 0)], COLORS, other, "?").items.map((x) => x.color), COLORS)
  const tok = splitParts([row("a", 0, 10), row("", 0, 30)], COLORS, other, "?")
  assert.equal(tok.cost, false)
  assert.deepEqual(tok.items.map((x) => [x.id, x.label, x.value]), [["-", "?", 30], ["a", "A", 10]])
})
