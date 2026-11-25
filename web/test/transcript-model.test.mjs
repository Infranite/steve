import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "typescript"

const src = readFileSync(new URL("../src/data/transcript-model.ts", import.meta.url), "utf8")
const code = ts.transpileModule(src, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText
const tm = {}
runInNewContext(code, {
  exports: tm,
  require: (id) => {
    if (id === "../i18n") return { __esModule: true, default: { language: "en", getResource: () => undefined, t: (k) => k }, t: (k) => k }
    throw new Error(`Unexpected require(${id})`)
  },
})

function sampleEvents() {
  const t0 = 1_700_000_000_000
  return [
    { seq: 1, ts: t0, kind: "user", data: { text: "Check module", via: "me" } },
    { seq: 2, ts: t0 + 10, kind: "text", data: { role: "thought", text: "Looking " } },
    { seq: 3, ts: t0 + 20, kind: "text", data: { role: "thought", text: "at files" } },
    { seq: 4, ts: t0 + 30, kind: "tool", data: { tool: { id: "c1", origin: "harness", name: "Edit", category: "edit", target: "a.ts", phase: "running" } } },
    { seq: 5, ts: t0 + 40, kind: "subtask", data: { subtask: { id: "sub1", title: "Sub", status: "running" } } },
    { seq: 6, ts: t0 + 50, kind: "tool", data: { tool: { id: "c1", origin: "harness", name: "Edit", category: "edit", target: "a.ts", phase: "succeeded", duration_ms: 20, result: [{ type: "diff", path: "a.ts", old: "x", new: "x\ny", added: 1, removed: 0 }] } } },
    { seq: 7, ts: t0 + 60, kind: "text", data: { role: "assistant", text: "First narration " } },
    { seq: 8, ts: t0 + 70, kind: "tool", data: { tool: { id: "c2", origin: "harness", name: "Read", category: "read", target: "b.ts", phase: "running" } } },
    { seq: 9, ts: t0 + 80, kind: "system", data: { text: "checkpoint" } },
    // Late tool update for c2 across a system boundary exercises the segment rebuild path
    { seq: 10, ts: t0 + 90, kind: "tool", data: { tool: { id: "c2", origin: "harness", name: "Read", category: "read", target: "b.ts", phase: "succeeded", result: [{ type: "text", text: "ok" }] } } },
    { seq: 11, ts: t0 + 100, kind: "subtask", data: { subtask: { id: "sub1", title: "Sub", status: "done" } } },
    { seq: 12, ts: t0 + 110, kind: "text", data: { role: "thought", text: "Summarizing" } },
    { seq: 13, ts: t0 + 120, kind: "text", data: { role: "assistant", text: "Answer " } },
    { seq: 14, ts: t0 + 130, kind: "text", data: { role: "assistant", text: "part 2 " } },
    { seq: 15, ts: t0 + 140, kind: "text", data: { role: "assistant", text: "part 3" } },
    { seq: 16, ts: t0 + 150, kind: "turn_end", data: { stopReason: "end_turn" } },
  ]
}

test("appendEvents matches mapTurns identically whether streamed one-by-one or in batches", () => {
  const all = sampleEvents()
  const bulk = tm.mapTurns(all)

  // One event per batch
  let one = { turns: tm.mapTurns(all.slice(0, 1)), oldest: null, newest: all[0].seq }
  for (let i = 1; i < all.length; i++) one = tm.appendEvents(one, [all[i]])

  // Batches of 3
  let three = { turns: tm.mapTurns(all.slice(0, 1)), oldest: null, newest: all[0].seq }
  for (let i = 1; i < all.length; i += 3) three = tm.appendEvents(three, all.slice(i, i + 3))

  const snap = (turns) => JSON.parse(JSON.stringify(turns.map(({ start, ev, seqs, ends, ts, rows }) => ({ start, ev, seqs, ends, ts, rows }))))
  assert.deepEqual(snap(one.turns), snap(bulk))
  assert.deepEqual(snap(three.turns), snap(bulk))
  // Cross-check against standalone segmentsOf reference
  assert.deepEqual(JSON.parse(JSON.stringify(one.turns[0].rows)), JSON.parse(JSON.stringify(tm.segmentsOf(one.turns[0]))))
})

test("streaming assistant tokens preserves earlier Seg identities including the preceding work block", () => {
  const t0 = 1_700_000_000_000
  const seed = [
    { seq: 1, ts: t0, kind: "user", data: { text: "Write answer", via: "me" } },
    { seq: 2, ts: t0 + 10, kind: "text", data: { role: "thought", text: "Thinking" } },
    { seq: 3, ts: t0 + 20, kind: "tool", data: { tool: { id: "c1", origin: "harness", name: "Read", category: "read", target: "a.ts", phase: "succeeded" } } },
    { seq: 4, ts: t0 + 30, kind: "text", data: { role: "assistant", text: "Start " } },
  ]
  let tr = { turns: tm.mapTurns(seed), oldest: null, newest: 4 }
  const userSeg = tr.turns[0].rows[0]
  const workSeg = tr.turns[0].rows[1]
  assert.equal(userSeg.t, "u")
  assert.equal(workSeg.t, "work")

  for (let seq = 5; seq <= 104; seq++) {
    tr = tm.appendEvents(tr, [{ seq, ts: t0 + seq * 10, kind: "text", data: { role: "assistant", text: `tok${seq} ` } }])
    assert.equal(tr.turns[0].rows[0], userSeg, `user seg identity preserved at seq ${seq}`)
    assert.equal(tr.turns[0].rows[1], workSeg, `work seg identity preserved at seq ${seq}`)
  }
  assert.equal(tr.turns[0].rows[2].t, "a")
  assert.ok(tr.turns[0].rows[2].evs[0][1].endsWith("tok104 "))
})
