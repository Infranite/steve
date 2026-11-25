import assert from "node:assert/strict"
import { test } from "node:test"

const t = await import("../src/data/table.ts")

const dec = (id, status = "open", q = "q") => ({ id, ws: "w", kind: "ask", subject_type: "test", subject_id: id, item_id: null, session_id: null, question: { kind: "raw", text: q }, draft: "", options: [], status, result: null, requested_by: "steve", created: 1, resolved: null, undo_until: null })
const up = (v) => ({ type: "decision", op: "upsert", id: v.id, view: v })
const rm = (id) => ({ type: "decision", op: "remove", id })
const ids = (table) => table.viewsOf("decision").map((d) => `${d.id}:${d.status}:${d.question.text}`).sort()

test("a snapshot's stale view arriving later in an older frame is dropped", () => {
  const x = new t.ViewTable()
  // X left the hot set at rev 90; the snapshot at rev 100 does not hold it; a replayed frame of rev 85 still has it open
  x.mergeSnapshot(100, { decision: [dec("A")] })
  x.mergePatch(85, [up(dec("X"))])
  assert.deepEqual(ids(x), ["A:open:q"])
  // a copy that is not hot (from a page) is kept, and the newest copy of anything wins
  x.mergePatch(85, [up(dec("Y", "resolved"))])
  x.mergePatch(120, [up(dec("A", "open", "changed"))])
  x.mergePatch(110, [up(dec("A", "open", "older"))])
  assert.deepEqual(ids(x), ["A:open:changed", "Y:resolved:q"])
  // a later snapshot drops the hot view it no longer holds, but not the page's copy
  x.mergeSnapshot(130, { decision: [] })
  assert.deepEqual(ids(x), ["Y:resolved:q"])
})

test("removals keep a tombstone, and a merge that changes nothing keeps versions and view identities", () => {
  const x = new t.ViewTable()
  x.mergePatch(10, [up(dec("A"))])
  const view = x.viewOf("decision", "A")
  const ver = x.version
  assert.equal(x.mergePatch(10, [up(dec("A"))]).changed, false)
  assert.equal(x.version, ver)
  const sameChange = x.mergePatch(11, [up(dec("A"))])
  assert.equal(sameChange.changed, true)
  assert.equal(sameChange.entities.size, 0, "rev-only update emits no entity content change")
  assert.equal(x.viewOf("decision", "A"), view, "an unchanged view keeps its object")
  x.mergePatch(12, [rm("A")])
  assert.deepEqual(ids(x), [])
  assert.equal(x.mergePatch(11, [up(dec("A"))]).changed, false, "older than its removal")
  x.mergePatch(13, [up(dec("A"))])
  assert.deepEqual(ids(x), ["A:open:q"])
})

test("frames, snapshots and reads by id in any order end as applying them by rev", () => {
  const sources = [], stored = new Map()
  let seed = 3
  const rand = (n) => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return (seed >>> 8) % n }
  for (let rev = 1; rev <= 80; rev++) {
    const id = "D" + rand(8), status = rand(3) ? "open" : "resolved"
    const kind = rand(4), view = dec(id, status, "r" + rev)
    if (kind === 3) {
      const snapshot = [...stored.values()].filter((d) => d.status === "open")
      sources.push({ rev, f: (x) => x.mergeSnapshot(rev, { decision: snapshot }) })
    } else if (kind === 1) {
      stored.delete(id)
      sources.push({ rev, f: (x) => x.mergePatch(rev, [rm(id)]) })
    } else {
      stored.set(id, view)
      sources.push(kind === 0 ? { rev, f: (x) => x.mergeViews(rev, "decision", [view]) }
        : { rev, f: (x) => x.mergePatch(rev, [up(view)]) })
    }
  }
  // A snapshot that excludes a reopened decision still supersedes its previous settlement.
  sources.push({ rev: 81, f: (x) => x.mergeViews(81, "decision", [dec("X", "resolved")]) })
  sources.push({ rev: 82, f: (x) => x.mergePatch(82, [up(dec("X"))]) })
  const lastSnapshot = [...stored.values()].filter((d) => d.status === "open")
  sources.push({ rev: 84, f: (x) => x.mergeSnapshot(84, { decision: lastSnapshot }) })
  const inOrder = new t.ViewTable()
  for (const s of sources) s.f(inOrder)
  for (let round = 0; round < 20; round++) {
    const shuffled = [...sources].sort(() => rand(3) - 1)
    const trial = new t.ViewTable()
    for (const s of shuffled) s.f(trial)
    assert.deepEqual(ids(trial), ids(inOrder))
  }
})

test("session reassignment and ending update the item and hot indexes without replacing other views", () => {
  const a = { id: "A", archived: false }
  const b = { id: "B", archived: false }
  const s = { id: "S", item_id: "A", status: "running" }
  const state = new t.ViewTable()
  state.mergeSnapshot(10, { item: [a, b], session: [s] })
  assert.deepEqual(state.viewsOf("session", "session:item:A"), [s])
  const ended = { ...s, item_id: "B", status: "ended" }
  state.mergePatch(12, [{ type: "session", op: "upsert", id: "S", view: ended }])
  assert.deepEqual(state.viewsOf("session", "session:item:A"), [])
  assert.deepEqual(state.viewsOf("session", "hot:session"), [])
  assert.deepEqual(state.viewsOf("session", "session:item:B"), [ended])
  assert.equal(state.viewOf("item", "A"), a)
  state.mergeSnapshot(13, { item: [b], session: [] })
  assert.equal(state.viewOf("item", "A"), undefined)
  assert.equal(state.viewOf("session", "S"), ended)
})

test("fine-grained kind, group and entity subscriptions isolate unrelated patches and preserve cached lists", () => {
  const state = new t.ViewTable()
  const a = { id: "A", archived: false, title: "a1" }
  const b = { id: "B", archived: false, title: "b1" }
  const s = { id: "S", item_id: "A", status: "running" }
  state.mergeSnapshot(1, { item: [a, b], session: [s] })

  const hits = { itemKind: 0, sessionKind: 0, itemA: 0, itemB: 0, hotDec: 0, itemASessions: 0 }
  state.subscribeKind("item", () => { hits.itemKind++ })
  state.subscribeKind("session", () => { hits.sessionKind++ })
  state.subscribeEntity("item", "A", () => { hits.itemA++ })
  state.subscribeEntity("item", "B", () => { hits.itemB++ })
  state.subscribeGroup("hot:decision", () => { hits.hotDec++ })
  state.subscribeGroup("session:item:A", () => { hits.itemASessions++ })

  const itemsBefore = state.viewsOf("item", "hot:item")
  const sessionsBefore = state.viewsOf("session", "hot:session")

  // Updating item A wakes only item kind + item:A, keeping session list reference identical.
  state.mergePatch(2, [{ type: "item", op: "upsert", id: "A", view: { ...a, title: "a2" } }])
  assert.deepEqual(hits, { itemKind: 1, sessionKind: 0, itemA: 1, itemB: 0, hotDec: 0, itemASessions: 0 })
  assert.notEqual(state.viewsOf("item", "hot:item"), itemsBefore)
  assert.equal(state.viewsOf("session", "hot:session"), sessionsBefore)

  // Re-applying an identical copy at a higher rev wakes no entity/kind/group subscribers.
  state.mergePatch(3, [{ type: "item", op: "upsert", id: "A", view: { id: "A", archived: false, title: "a2" } }])
  assert.deepEqual(hits, { itemKind: 1, sessionKind: 0, itemA: 1, itemB: 0, hotDec: 0, itemASessions: 0 })

  // deepEqual handles nested objects, arrays, and omitted vs undefined keys identically to JSON.stringify.
  assert.equal(t.deepEqual({ a: 1, b: [2, { c: 3 }], d: undefined }, { b: [2, { c: 3 }], a: 1 }), true)
  assert.equal(t.deepEqual({ a: 1, b: [2, 3] }, { a: 1, b: [2, 4] }), false)
  assert.equal(t.deepEqual([1, 2], { 0: 1, 1: 2 }), false)
})
