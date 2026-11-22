import assert from "node:assert/strict"
import { test } from "node:test"

const t = await import("../src/data/table.ts")
const { minCursor } = await import("../src/stream-cursor.ts")

const dec = (id, status = "open", q = "q") => ({ id, ws: "w", kind: "ask", subject_type: "test", subject_id: id, item_id: null, session_id: null, question: q, draft: "", options: [], status, result: null, requested_by: "steve", created: 1, resolved: null, undo_until: null })
const up = (v) => ({ type: "decision", op: "upsert", id: v.id, view: v })
const rm = (id) => ({ type: "decision", op: "remove", id })
const ids = (table) => t.viewsOf(table, "decision").map((d) => `${d.id}:${d.status}:${d.question}`).sort()

test("a snapshot's stale view arriving later in an older frame is dropped", () => {
  // X left the hot set at rev 90; the snapshot at rev 100 does not hold it; a replayed frame of rev 85 still has it open
  let x = t.mergeSnapshot(t.EMPTY, 100, { decision: [dec("A")] })
  x = t.mergePatch(x, 85, [up(dec("X"))])
  assert.deepEqual(ids(x), ["A:open:q"])
  // a copy that is not hot (from a page) is kept, and the newest copy of anything wins
  x = t.mergePatch(x, 85, [up(dec("Y", "resolved"))])
  x = t.mergePatch(x, 120, [up(dec("A", "open", "changed"))])
  x = t.mergePatch(x, 110, [up(dec("A", "open", "older"))])
  assert.deepEqual(ids(x), ["A:open:changed", "Y:resolved:q"])
  // a later snapshot drops the hot view it no longer holds, but not the page's copy
  x = t.mergeSnapshot(x, 130, { decision: [] })
  assert.deepEqual(ids(x), ["Y:resolved:q"])
})

test("removals keep a tombstone, and a merge that changes nothing keeps the table and its views", () => {
  let x = t.mergePatch(t.EMPTY, 10, [up(dec("A"))])
  const view = t.viewOf(x, "decision", "A")
  assert.equal(t.mergePatch(x, 10, [up(dec("A"))]), x)
  const same = t.mergePatch(x, 11, [up(dec("A"))])
  assert.equal(t.viewOf(same, "decision", "A"), view, "an unchanged view keeps its object")
  x = t.mergePatch(same, 12, [rm("A")])
  assert.deepEqual(ids(x), [])
  assert.equal(t.mergePatch(x, 11, [up(dec("A"))]), x, "older than its removal")
  assert.deepEqual(ids(t.mergePatch(x, 13, [up(dec("A"))])), ["A:open:q"])
})

test("frames, snapshots and reads by id in any order end as applying them by rev", () => {
  const sources = []
  let seed = 3
  const rand = (n) => { seed = (seed * 1103515245 + 12345) % 2147483648; return seed % n }
  for (let rev = 1; rev <= 40; rev++) {
    const id = "D" + rand(5), status = rand(3) ? "open" : "resolved"
    const kind = rand(4)
    sources.push(kind === 0 ? { rev, f: (x) => t.mergeViews(x, rev, "decision", [dec(id, status, "r" + rev)]) }
      : kind === 1 ? { rev, f: (x) => t.mergePatch(x, rev, [rm(id)]) }
      : { rev, f: (x) => t.mergePatch(x, rev, [up(dec(id, status, "r" + rev))]) })
  }
  const inOrder = sources.reduce((x, s) => s.f(x), t.EMPTY)
  for (let round = 0; round < 20; round++) {
    const shuffled = [...sources].sort(() => rand(3) - 1)
    assert.deepEqual(ids(shuffled.reduce((x, s) => s.f(x), t.EMPTY)), ids(inOrder))
  }
})

test("the stream continues from the older of two cursors of one epoch", () => {
  assert.equal(minCursor("ep_a:10", "ep_a:7"), "ep_a:7")
  assert.equal(minCursor(null, "ep_a:7"), "ep_a:7")
  assert.equal(minCursor("ep_a:10", "ep_b:7"), null)
})
