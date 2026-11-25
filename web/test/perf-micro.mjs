// Deterministic load-independent micro-benchmarks for transcript append and ViewTable patching.
// Run via `npm run bench:web:micro` (not part of CI default tests).
import { readFileSync } from "node:fs"
import { createRequire } from "node:module"
import { resolve } from "node:path"
import { performance } from "node:perf_hooks"
import { fileURLToPath } from "node:url"
import { runInNewContext } from "node:vm"

const root = resolve(fileURLToPath(new URL("../..", import.meta.url)))
const ts = createRequire(resolve(root, "web/package.json"))("typescript")

function loadTs(rel, mocks = {}) {
  const src = readFileSync(resolve(root, rel), "utf8")
  const code = ts.transpileModule(src, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
  }).outputText
  const exports = {}
  const mod = { exports }
  runInNewContext(code, {
    exports,
    module: mod,
    require: (id) => {
      if (id in mocks) return mocks[id]
      throw new Error(`Unmocked require(${id}) in ${rel}`)
    },
    console,
    performance,
    Date,
    Math,
    Map,
    Set,
    WeakMap,
    JSON,
    Object,
    Array,
    String,
    Number,
    Boolean,
    RegExp,
    Error,
    Promise,
    setTimeout,
    clearTimeout,
  })
  return mod.exports
}

const pct = (xs, q) => {
  if (!xs.length) return 0
  const s = [...xs].sort((a, b) => a - b)
  return Math.round(s[Math.min(s.length - 1, Math.ceil(s.length * q) - 1)] * 1000) / 1000
}
const r2 = (n) => Math.round(n * 100) / 100

// 1. Transcript incremental append benchmark (2,000 events in 500 frames of 4 events)
function benchTranscript() {
  const tm = loadTs("web/src/data/transcript-model.ts", {
    "../i18n": { __esModule: true, default: { language: "en", getResource: () => undefined, t: (k) => k }, t: (k) => k },
  })
  const baseTs = 1700000000000
  const all = [{ seq: 1, ts: baseTs, kind: "user", data: { text: "Audit performance across the workspace", via: "me" } }]
  let seq = 1
  for (let i = 0; i < 200; i++) {
    all.push({ seq: ++seq, ts: baseTs + seq * 10, kind: "text", data: { role: "thought", text: `Thinking step ${i} about module ${i % 17}. ` } })
    const cid = `call-${i}`
    all.push({ seq: ++seq, ts: baseTs + seq * 10, kind: "tool", data: { tool: { id: cid, origin: "harness", name: i % 3 === 0 ? "Edit" : "Read", category: i % 3 === 0 ? "edit" : "read", title: `File ${i}`, target: `/repo/src/mod${i % 30}.ts`, phase: "running" } } })
    all.push({ seq: ++seq, ts: baseTs + seq * 10, kind: "tool", data: { tool: { id: cid, origin: "harness", name: i % 3 === 0 ? "Edit" : "Read", category: i % 3 === 0 ? "edit" : "read", title: `File ${i}`, target: `/repo/src/mod${i % 30}.ts`, phase: "succeeded", duration_ms: 42, result: i % 3 === 0 ? [{ type: "diff", path: `/repo/src/mod${i % 30}.ts`, old: "a\nb", new: "a\nc\nd", added: 2, removed: 1 }] : [{ type: "text", text: "file contents line\n".repeat(10) }] } } })
    if (i % 25 === 24) {
      all.push({ seq: ++seq, ts: baseTs + seq * 10, kind: "system", data: { text: `Checkpoint ${i}` } })
    }
  }
  for (let i = 0; i < 1400; i++) {
    all.push({ seq: ++seq, ts: baseTs + seq * 10, kind: "text", data: { role: "assistant", text: `token_${i} ` } })
  }

  let tr = { turns: tm.mapTurns(all.slice(0, 1)), oldest: null, newest: all[0].seq }
  const frameMs = []
  let preservedUserSeg = 0
  let totalChecks = 0
  const firstRow0 = tr.turns[0].rows[0]
  const t0 = performance.now()
  for (let i = 1; i < all.length; i += 4) {
    const batch = all.slice(i, i + 4)
    const s = performance.now()
    tr = tm.appendEvents(tr, batch)
    frameMs.push(performance.now() - s)
    totalChecks++
    if (tr.turns[0].rows[0] === firstRow0) preservedUserSeg++
  }
  const totalMs = performance.now() - t0
  return {
    total_events: all.length,
    batches: frameMs.length,
    total_ms: r2(totalMs),
    batch_p50_ms: pct(frameMs, 0.5),
    batch_p95_ms: pct(frameMs, 0.95),
    batch_max_ms: pct(frameMs, 1),
    preserved_earlier_seg_identity_pct: r2((preservedUserSeg / totalChecks) * 100),
    final_rows: tr.turns[0].rows.length,
    final_tuples: tr.turns[0].ev.length,
  }
}

// 2. ViewTable mergePatch benchmark (1,500 existing rows, 2,000 single-row patches)
function benchViewTable() {
  const tbl = loadTs("web/src/data/table.ts")
  const makeMsg = (i) => ({
    id: `m-${i}`, ws: "ws1", author: i % 2 ? "steve" : "me", item_id: null,
    text: "Markdown body paragraph with details.\n".repeat(8),
    created: 1700000000000 + i * 1000,
    meta: i % 5 === 0 ? {
      steps: {
        research: { run: `run-${i}`, ms: 12000, turns: 2, tools: 8 },
        tools: Array.from({ length: 8 }, (_, j) => ({
          name: "Read", kind: "read", call: j, args: { brief: `file-${j}.ts` },
          result: { lines: Array.from({ length: 20 }, (_, k) => `line ${k}`) }, status: "ok", ms: 40,
        })),
      },
    } : null,
  })
  const makeItem = (i) => ({
    id: `ST-${i}`, ws: "ws1", no: i, title: `Work item ${i}`, bot_id: "dev",
    state: { kind: "active" }, cycle: 1, revision: 1, archived: false, star: false,
    priority: "中", created: 1700000000000 + i * 100, updated: 1700000000000 + i * 100,
    tags: ["t1", "t2"], deps: [], waits: [], delivery: [],
  })
  const makeSession = (i) => ({
    id: `ses-${i}`, ws: "ws1", no: 1, item_id: `ST-${i % 300}`, bot_id: "dev",
    status: i % 3 === 0 ? "running" : "idle", title: `Session ${i}`, created: 1700000000000, updated: 1700000000000 + i,
    queued: 0, busy: i % 3 === 0, readonly: false,
  })
  const makeDecision = (i) => ({
    id: `dec-${i}`, ws: "ws1", kind: "approve", subject_type: "item", subject_id: `ST-${i % 300}`,
    item_id: `ST-${i % 300}`, session_id: `ses-${i % 300}`, question: { kind: "raw", text: `Approve ${i}?` },
    draft: "Details...", options: ["yes", "no"], status: "open", result: null, requested_by: "dev",
    created: 1700000000000 + i, resolved: null, undo_until: null, tier: "decide", revision: 1,
  })

  const snap = {
    message: { views: Array.from({ length: 600 }, (_, i) => makeMsg(i)), next: null },
    item: Array.from({ length: 300 }, (_, i) => makeItem(i)),
    session: Array.from({ length: 300 }, (_, i) => makeSession(i)),
    decision: Array.from({ length: 300 }, (_, i) => makeDecision(i)),
  }

  // Support both pure table API (baseline) and mutable versioned ViewTable class
  const isClass = typeof tbl.ViewTable === "function"
  let state = isClass ? new tbl.ViewTable() : tbl.EMPTY
  if (isClass) state.mergeSnapshot(1, snap)
  else state = tbl.mergeSnapshot(state, 1, snap)

  const patchMs = []
  let rev = 1
  const t0 = performance.now()
  for (let i = 0; i < 2000; i++) {
    rev++
    const kind = i % 4
    let op
    if (kind === 0) {
      const idx = i % 600
      const m = makeMsg(idx)
      m.text += ` rev ${rev}`
      op = { type: "message", op: "upsert", id: m.id, view: m }
    } else if (kind === 1) {
      const idx = i % 300
      const it = makeItem(idx)
      it.updated += rev
      it.star = rev % 2 === 0
      op = { type: "item", op: "upsert", id: it.id, view: it }
    } else if (kind === 2) {
      const idx = i % 300
      const s = makeSession(idx)
      s.updated += rev
      op = { type: "session", op: "upsert", id: s.id, view: s }
    } else {
      // identical view (tests equality fast-path)
      const idx = i % 600
      const m = makeMsg(idx)
      op = { type: "message", op: "upsert", id: m.id, view: m }
    }
    const s = performance.now()
    if (isClass) state.mergePatch(rev, [op])
    else state = tbl.mergePatch(state, rev, [op])
    patchMs.push(performance.now() - s)
  }
  const totalMs = performance.now() - t0
  return {
    initial_rows: 1500,
    patches: 2000,
    total_ms: r2(totalMs),
    patch_p50_ms: pct(patchMs, 0.5),
    patch_p95_ms: pct(patchMs, 0.95),
    patch_max_ms: pct(patchMs, 1),
  }
}

console.log(JSON.stringify({
  transcript_append: benchTranscript(),
  view_table_patch: benchViewTable(),
}, null, 2))
