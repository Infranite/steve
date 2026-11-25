import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { runInNewContext } from "node:vm"
import ts from "typescript"

const root = new URL("..", import.meta.url).pathname
const srcPath = join(root, "src/data/transcript-model.ts")
const code = ts.transpileModule(readFileSync(srcPath, "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText
const exportsObj = {}
runInNewContext(code, {
  exports: exportsObj,
  require: (id) => {
    if (id === "../i18n") {
      const t = (k) => k
      return { __esModule: true, default: { language: "en", getResource: () => undefined, t }, t }
    }
    throw new Error(`unexpected import: ${id}`)
  },
})
const {
  activeSubtasksOf,
  mapTurns,
  partitionWork,
  subtaskOf,
  subtaskTimes,
  toolDurationMs,
  toolId,
  toolParent,
} = exportsObj

const ev = (seq, ts, kind, data) => ({ seq, ts, kind, data })

test("subtask events update in place, preserve timestamps, nest child tools, and hide duplicate launcher rows", () => {
  const events = [
    ev(1, 1000, "user", { text: "check repo", via: "me" }),
    ev(2, 1100, "tool", { tool: { id: "call-sub", origin: "harness", name: "subagent", category: "other", title: "subagent", phase: "running" } }),
    ev(3, 1110, "subtask", { subtask: { id: "call-sub", origin: "subagent", status: "running", title: "Audit auth", actor: "reviewer", can_cancel: true } }),
    ev(4, 1200, "tool", { tool: { id: "child-read", parent: "call-sub", origin: "harness", name: "Read", category: "read", title: "Read auth.rs", target: "auth.rs", phase: "succeeded", duration_ms: 85 } }),
    ev(5, 1300, "subtask", { subtask: { id: "bg-1", origin: "background_task", status: "running", title: "Run cargo test", actor: "Bash", can_cancel: false } }),
    ev(6, 1800, "subtask", { subtask: { id: "call-sub", origin: "subagent", status: "done", title: "Audit auth", actor: "reviewer", can_cancel: false, duration_ms: 690, summary: "No issues found", usage: { input: 400, output: 120, cost: { decimal: "0.004", currency: "USD" } } } }),
  ]

  const turns = mapTurns(events)
  assert.equal(turns.length, 1)
  const work = turns[0].rows.find((r) => r.t === "work")
  assert.ok(work, "expected work segment containing subtasks")

  const part = partitionWork(work.evs)
  assert.deepEqual(Array.from(part.subtaskIds).sort(), ["bg-1", "call-sub"])
  // Launcher tool `call-sub` and child tool `child-read` are not duplicated at root level
  const rootSubIds = Array.from(part.rootIndices).map((i) => subtaskOf(work.evs[i])?.id).filter(Boolean)
  assert.deepEqual(rootSubIds, ["call-sub", "bg-1"])
  const rootToolIds = Array.from(part.rootIndices).map((i) => toolId(work.evs[i])).filter(Boolean)
  assert.deepEqual(rootToolIds, [])

  // Child tool is nested under `call-sub` and preserves parent + duration_ms
  const childIndices = Array.from(part.childrenBySubtask.get("call-sub") ?? [])
  assert.equal(childIndices.length, 1)
  const childEv = work.evs[childIndices[0]]
  assert.equal(toolId(childEv), "child-read")
  assert.equal(toolParent(childEv), "call-sub")
  assert.equal(toolDurationMs(childEv), 85)

  // In-place update preserved started=1110 and recorded ended=1800
  const subEv = work.evs[part.rootIndices[0]]
  assert.equal(subtaskOf(subEv)?.status, "done")
  assert.equal(subtaskOf(subEv)?.usage?.output, 120)
  assert.deepEqual({ ...subtaskTimes(subEv) }, { started: 1110, ended: 1800 })

  // Only `bg-1` remains active for the header RunningNow capsule
  const active = activeSubtasksOf({ turns, oldest: null, newest: 6 })
  assert.equal(active.length, 1)
  assert.equal(active[0].subtask.id, "bg-1")
  assert.equal(active[0].subtask.can_cancel, false)
})

test("web transcript and turn-trace contain no hardcoded Task/Agent subagent detection", () => {
  for (const rel of ["src/thread/transcript.tsx", "src/thread/turn-trace.tsx"]) {
    const src = readFileSync(join(root, rel), "utf8")
    assert.ok(!src.includes("DELEGATE"), `${rel} must not define hardcoded DELEGATE list`)
    assert.ok(!/["']Task["']/.test(src), `${rel} must not branch on literal "Task"`)
    assert.ok(!/["']Agent["']/.test(src), `${rel} must not branch on literal "Agent"`)
  }
})
