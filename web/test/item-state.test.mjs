import assert from "node:assert/strict"
import test from "node:test"
import { isClosedItemState, itemDisplayStatusOf, itemState, itemStateSublabel } from "../src/item-state.ts"
import enViews from "../src/i18n/locales/en/views.ts"
import zhViews from "../src/i18n/locales/zh-CN/views.ts"

const ALL_STATE_CASES = [
  {
    kind: "done",
    input: { st: "done" },
    running: false,
    tone: "done",
    move: "none",
    display: "done",
    closed: true,
    sublabel: null,
  },
  {
    kind: "dropped",
    input: { st: "drop" },
    running: false,
    tone: "neutral",
    move: "none",
    display: "drop",
    closed: true,
    sublabel: null,
  },
  {
    kind: "failed",
    input: { st: "fail", progress: "构建失败" },
    running: false,
    tone: "fail",
    move: "none",
    display: "fail",
    closed: true,
    sublabel: null,
  },
  {
    kind: "proposed",
    input: { st: "prop" },
    running: false,
    tone: "wait",
    move: "you",
    display: "prop",
    closed: false,
    sublabel: null,
  },
  {
    kind: "asking",
    input: { st: "wait", waits: [{ q: "确认继续吗？" }] },
    running: false,
    tone: "wait",
    move: "you",
    display: "wait",
    closed: false,
    sublabel: null,
  },
  {
    kind: "dispatching",
    input: { st: "run", dispatching: true },
    running: false,
    tone: "run",
    move: "steve",
    display: "run",
    closed: false,
    sublabel: null,
  },
  {
    kind: "paused",
    input: { st: "pause" },
    running: false,
    tone: "pause",
    move: "you",
    display: "pause",
    closed: false,
    sublabel: "you",
  },
  {
    kind: "blocked",
    input: { st: "block", progress: "等待前置事项" },
    running: false,
    tone: "pause",
    move: "agents",
    display: "block",
    closed: false,
    sublabel: null,
  },
  {
    kind: "working",
    input: { st: "run", progress: "写代码" },
    running: true,
    tone: "run",
    move: "agents",
    display: "run",
    closed: false,
    sublabel: null,
  },
  {
    kind: "queued",
    input: { st: "run", queued: 1 },
    running: false,
    tone: "run",
    move: "agents",
    display: "run",
    closed: false,
    sublabel: "waitingAgent",
  },
  {
    kind: "closing",
    input: { st: "run", phase: "closing" },
    running: false,
    tone: "run",
    move: "steve",
    display: "run",
    closed: false,
    sublabel: null,
  },
  {
    kind: "idle",
    input: { st: "run", progress: "等它继续" },
    running: false,
    tone: "neutral",
    move: "agents",
    display: "run",
    closed: false,
    sublabel: null,
  },
]

test("a card for you comes before anything running; running comes before paused or idle", () => {
  assert.equal(itemState({ st: "run", waits: [{ q: "批准吗？" }] }, true).kind, "asking")
  assert.equal(itemState({ st: "run", progress: "写代码" }, true).kind, "working")
  assert.equal(itemState({ st: "run", q: [{}] }, false).kind, "queued")
  assert.equal(itemDisplayStatusOf(itemState({ st: "run", waits: [{ q: "批准吗？" }] }, true)), "wait")
})

test("lifecycle states", () => {
  assert.equal(itemState({ st: "done" }, false).move, "none")
  assert.equal(itemState({ st: "fail", progress: "构建失败" }, false).detail, "构建失败")
  assert.equal(itemState({ st: "pause" }, false).kind, "paused")
  assert.equal(itemState({ st: "block" }, false).kind, "blocked")
  assert.equal(itemState({ st: "run", q: [{}] }, false).kind, "queued")
  assert.equal(itemState({ st: "run", progress: "等它继续" }, false).kind, "idle")
})

test("the cycle shows as closing without a session, including during delivery verification", () => {
  assert.equal(itemState({ st: "run", phase: "closing" }, false).kind, "closing")
})

test("an unassigned item awaiting async Steve bot dispatch shows as dispatching with move steve", () => {
  const s = itemState({ st: "run", dispatching: true }, false)
  assert.equal(s.kind, "dispatching")
  assert.equal(s.tone, "run")
  assert.equal(s.move, "steve")
  assert.equal(itemDisplayStatusOf(s), "run")
})

test("all 12 StateKinds derive accurate state, closed check, and non-misleading sublabels", () => {
  for (const c of ALL_STATE_CASES) {
    const s = itemState(c.input, c.running)
    assert.equal(s.kind, c.kind, `${c.kind}: kind`)
    assert.equal(s.tone, c.tone, `${c.kind}: tone`)
    assert.equal(s.move, c.move, `${c.kind}: move`)
    assert.equal(itemDisplayStatusOf(s), c.display, `${c.kind}: display`)
    assert.equal(isClosedItemState(s), c.closed, `${c.kind}: isClosedItemState`)
    assert.equal(itemStateSublabel(s), c.sublabel, `${c.kind}: itemStateSublabel`)
  }
})

test("itemState locales define all 12 headlines, accurate sublabels, and drop misleading agent wording", () => {
  for (const c of ALL_STATE_CASES) {
    assert.equal(typeof zhViews.itemState[c.kind], "string", `zh-CN missing headline for ${c.kind}`)
    assert.ok(zhViews.itemState[c.kind].trim().length > 0, `zh-CN empty headline for ${c.kind}`)
    assert.equal(typeof enViews.itemState[c.kind], "string", `en missing headline for ${c.kind}`)
    assert.ok(enViews.itemState[c.kind].trim().length > 0, `en empty headline for ${c.kind}`)
  }

  assert.equal(zhViews.itemState.you, "该你了")
  assert.equal(enViews.itemState.you, "Your move")
  assert.equal(zhViews.itemState.waitingAgent, "等 Agent 接手")
  assert.equal(enViews.itemState.waitingAgent, "Waiting for an agent")

  assert.equal("agents" in zhViews.itemState, false)
  assert.equal("agents" in enViews.itemState, false)
  assert.equal("steve" in zhViews.itemState, false)
  assert.equal("steve" in enViews.itemState, false)

  for (const [lang, dict] of [["zh-CN", zhViews.itemState], ["en", enViews.itemState]]) {
    for (const [k, v] of Object.entries(dict)) {
      assert.doesNotMatch(String(v), /等 agent 处理|With the agents/, `${lang} itemState.${k} keeps misleading wording`)
    }
  }
})
