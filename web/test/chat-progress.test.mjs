import assert from "node:assert/strict"
import { test } from "node:test"
import {
  SILENCE_HINT_THRESHOLD_S,
  isResearchSnapshot,
  liveRunFromSnapshot,
  recoveredCalls,
  researchEvent,
  researchProgressEvent,
} from "../src/thread/chat-progress.ts"

const raw = { type: "steve.tool", mid: "msg-1", run: "run-1", ws: "test", name: "Read", kind: "read", brief: "notes.md", ok: true }
const event = (l, changes) => researchEvent(l, { ...raw, ...changes })

test("a running tool is not a success and a different call's completion cannot clear it", () => {
  let live = event(null, { call: 1, running: true, started_at: 100, at: 1000 })
  live = event(live, { call: 2, running: true, at: 1010 })
  assert.equal(live.tools.length, 0)
  assert.equal(live.now.call, 2)
  live = event(live, { call: 2, ok: false, at: 1020 })
  assert.equal(live.now.call, 1)
  assert.equal(live.startedAt, 100)
  assert.equal(live.at, 1020)
  assert.equal(live.tools[0].ok, false)
})

test("recovery requires a success on the same stable call; legacy rows remain failures", () => {
  let live = event(null, { call: 1, ok: false })
  live = event(live, { call: 2, ok: true })
  live = event(live, { ok: false })
  assert.deepEqual([...recoveredCalls(live.tools, row => row.ok)], [])
  live = event(live, { call: 1, running: true })
  assert.deepEqual([...recoveredCalls(live.tools, row => row.ok)], [])
  live = event(live, { call: 1 })
  live = event(live, { call: 1 })
  assert.equal(live.tools.length, 4, "same terminal view is not duplicated")
  assert.deepEqual([...recoveredCalls(live.tools, row => row.ok)], [0])
  live = event(live, { run: "run-2", call: 1 })
  assert.equal(live.tools.length, 1)
  assert.deepEqual([...recoveredCalls(live.tools, row => row.ok)], [])
  assert.equal(live.startedAt, undefined)
  assert.equal(live.at, undefined)
})


test("tool changes leave the last published snapshot intact for frame-batched readers", () => {
  const before = event(null, { call: 1, running: true })
  const after = event(before, { call: 1 })
  assert.equal(before.now.call, 1)
  assert.deepEqual(before.tools, [])
  assert.equal(after.now, undefined)
  assert.equal(after.tools[0].call, 1)
})

test("progress events keep turn startedAt, accumulate thoughts/plan/subtasks, and restore from snapshot", () => {
  assert.equal(SILENCE_HINT_THRESHOLD_S, 15)
  let live = researchProgressEvent(null, {
    type: "steve.progress",
    ws: "test",
    mid: "msg-1",
    run: "run-9",
    started_at: 1000,
    at: 1200,
    body: {
      kind: "plan",
      entries: [
        { content: "Locate bug", status: "completed" },
        { content: "Fix timer", status: "in_progress" },
      ],
    },
  })
  assert.equal(live.startedAt, 1000)
  assert.equal(live.plan?.length, 2)

  live = researchProgressEvent(live, {
    type: "steve.progress",
    ws: "test",
    mid: "msg-1",
    run: "run-9",
    started_at: 1000,
    at: 1400,
    body: { kind: "thought", id: "th-1", text: "First part", done: false },
  })
  assert.equal(live.stage, "thinking")
  assert.equal(live.thoughts?.length, 1)

  live = researchProgressEvent(live, {
    type: "steve.progress",
    ws: "test",
    mid: "msg-1",
    run: "run-9",
    started_at: 1000,
    at: 1600,
    body: { kind: "thought", id: "th-1", text: "First part + second part", done: true },
  })
  assert.equal(live.thoughts?.[0].startedAt, 1400)
  assert.equal(live.thoughts?.[0].at, 1600)
  assert.equal(live.thoughts?.[0].done, true)
  assert.equal(live.stage, "responding")

  live = researchEvent(live, {
    type: "steve.tool",
    ws: "test",
    mid: "msg-1",
    run: "run-9",
    call: 1,
    name: "Read",
    kind: "read",
    brief: "src/chat2.tsx",
    description: "Inspect chat progress header",
    running: true,
    ok: true,
    started_at: 1000,
    step_started_at: 1700,
    at: 1700,
  })
  assert.equal(live.stage, "reading")
  assert.equal(live.now?.startedAt, 1700)
  assert.equal(live.now?.description, "Inspect chat progress header")

  live = researchEvent(live, {
    type: "steve.tool",
    ws: "test",
    mid: "msg-1",
    run: "run-9",
    call: 1,
    name: "Read",
    kind: "read",
    brief: "src/chat2.tsx",
    description: "Inspect chat progress header",
    running: false,
    ok: true,
    started_at: 1000,
    step_started_at: 1700,
    at: 2100,
    ms: 400,
    excerpt: "export function LiveSteps()",
  })
  assert.equal(live.stage, "responding")
  assert.equal(live.tools[0].ms, 400)
  assert.equal(live.tools[0].excerpt, "export function LiveSteps()")
  assert.equal(live.startedAt, 1000)

  live = researchProgressEvent(live, {
    type: "steve.progress",
    ws: "test",
    mid: "msg-1",
    run: "run-9",
    started_at: 1000,
    at: 2200,
    body: {
      kind: "subtask",
      subtask: {
        id: "sub-1",
        origin: "subagent",
        status: "running",
        title: "Check locales",
        actor: "Explore",
        progress: "Scanning zh-CN/en",
        started_at: 2200,
      },
    },
  })
  assert.equal(live.subtasks?.length, 1)

  const hydrated = liveRunFromSnapshot({
    run: "run-9",
    mid: "msg-1",
    started_at: 1000,
    last_activity_at: 2500,
    stage: "reading",
    plan: [{ content: "Inspect codebase", status: "in_progress" }],
    thoughts: [{ id: "t1", text: "Checking state", started_at: 1200, at: 1500, done: true }],
    tools: [
      { call: 1, name: "Read", kind: "read", brief: "a.ts", description: "Read A", running: false, ok: true, started_at: 1600, at: 1800, ms: 200, excerpt: "const a = 1" },
      { call: 2, name: "Bash", kind: "exec", brief: "npm test", description: "Run suite", running: true, ok: true, started_at: 2000, at: 2000 },
    ],
    subtasks: [{ id: "s1", origin: "subagent", status: "running", title: "Audit", started_at: 2100 }],
    draft_text: "",
    draft_thought: "",
  })
  assert.equal(hydrated.startedAt, 1000)
  assert.equal(hydrated.at, 2500)
  assert.equal(hydrated.stage, "reading")
  assert.equal(hydrated.tools.length, 1)
  assert.equal(hydrated.tools[0].excerpt, "const a = 1")
  assert.equal(hydrated.now?.call, 2)
  assert.equal(hydrated.now?.startedAt, 2000)
  assert.equal(hydrated.subtasks?.length, 1)

  assert.equal(isResearchSnapshot({
    run: "run-ans", mid: "m-1", started_at: 1000, last_activity_at: 1200,
    phase: "answer", plan: [], thoughts: [], tools: [], subtasks: [], draft_text: "", draft_thought: "thinking",
  }), false)
  assert.equal(isResearchSnapshot({
    run: "run-res", mid: "m-2", started_at: 1000, last_activity_at: 1200,
    phase: "research", plan: [], thoughts: [], tools: [], subtasks: [], draft_text: "", draft_thought: "",
  }), true)
})
