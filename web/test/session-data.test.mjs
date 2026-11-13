import assert from "node:assert/strict"
import { test } from "node:test"
import { filesOf, outputsOf, turnsOf, withUsage, workedMs } from "../src/session/data.ts"

const ev = [["u", "fix the bug"], ["th", "…"], ["tool", "edit", "Edit", "a.ts", "", 0, 3, 1, 0], ["tool", "run", "Bash", "ls", "", 0, null, null, 1], ["a", "done"], ["end", "end_turn"],
  ["hi", "brief", null, "Steve brief\nmore"], ["u", "steer"], ["tool", "edit", "Edit", "a.ts", "", 0, 2, 0, 0], ["end", "cancelled"], ["u", "again"], ["th", "x"]]
const ts = [100, 110, 120, 130, 140, 200, 300, 310, 320, 400, 500, 510]

test("turns run from the prompt that opened them to the harness's end; a message inside a turn steers it", () => {
  const t = turnsOf(ev, ts, true)
  assert.equal(t.length, 3)
  assert.deepEqual(t.map((x) => [x.by, x.start, x.end, x.tools, x.fails, x.stop, x.live]), [["u", 100, 200, 2, 1, "end_turn", false], ["hi", 300, 400, 1, 0, "cancelled", false], ["u", 500, undefined, 0, 0, undefined, true]])
  assert.equal(t[1].prompt, "Steve brief more")
  assert.equal(workedMs(t, 600), 100 + 100 + 100)
})

test("an imported transcript has no ends: a prompt after the work opens the next turn", () => {
  const imp = [["u", "a"], ["a", "x"], ["u", "b"], ["tool", "run", "Bash", "ls"], ["u", "c"]]
  assert.equal(turnsOf(imp, [1, 1, 1, 1, 1], false).length, 1)
  assert.equal(turnsOf(imp, [1, 1, 1, 1, 1], false, undefined, true).length, 3)
})

test("a turn takes the usage of the stats turn that started and ended with it, within 5 s", () => {
  const t = turnsOf(ev, ts.map((x) => x * 1000), false)
  const u = { input: 1, output: 2, cache_read: 3, cache_write: 0, total: 6, est: 0.1, reported: 0, unpriced_tokens: 0 }
  const got = withUsage(t, [{ session_id: "s", started: 102_000, ended: 199_000, usage: u }, { session_id: "other", started: 300_000, ended: 400_000, usage: u }], "s")
  assert.deepEqual(got.map((x) => x.usage?.total), [6, undefined, undefined])
})

test("files changed add up per path, failed edits left out, most changed first", () => {
  assert.deepEqual(filesOf([...ev, ["tool", "edit", "Edit", "b.ts", "", 0, 9, 9, 0], ["tool", "edit", "Edit", "c.ts", "", 0, 5, 5, 1]]), [["b.ts", 9, 9], ["a.ts", 5, 1]])
})

test("a session's outputs are the ones delivered with a timeline line naming it", () => {
  const arts = [{ id: "a1", created: 1000 }, { id: "a2", created: 5000 }, { id: "a3", created: 9000 }]
  const tl = [{ ts: 1000, session_id: "s1" }, { ts: 5001, session_id: "s2" }]
  assert.deepEqual(outputsOf("s1", arts, tl, ["s1", "s2"]).map((a) => a.id), ["a1"])
  /* no line names a session at that moment: only the item's one session gets it */
  assert.deepEqual(outputsOf("s1", arts, tl, ["s1"]).map((a) => a.id), ["a1", "a3"])
})
