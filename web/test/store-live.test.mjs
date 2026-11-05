import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "../node_modules/typescript/lib/typescript.js"
import * as pages from "../src/event-pages.ts"
import * as live from "../src/live-utils.ts"
import * as notes from "../src/decision-notes.ts"
import * as feed from "../src/feed-order.ts"
import * as format from "../src/i18n/format.ts"
import * as i18n from "../src/i18n/index.ts"

const code = ts.transpileModule(readFileSync(new URL("../src/store.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve() }
const defer = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b }); return { promise, resolve, reject } }
const session = (id) => ({ id, no: 1, item_id: null, status: "running", created: 1, updated: 2, queued: [], harness: "codex" })
function harness() {
  const state = { workspace: { id: "ws", item_prefix: "ST" }, workspaces: [], items: [], bots: [], allBots: [], projects: [], sessions: [session("a"), session("b")], decisions: [], recent: [], thread: [], tags: [], tagUses: {}, settings: {}, devices: [], runtimes: [] }
  let onEvent, onOpen, cursorFn, opens = 0
  const marks = new WeakMap(), wm = { cursor: null }
  const timers = [], cues = [], notices = [], pageWs = []
  const api = { state: async () => { const s = structuredClone(state); if (wm.cursor) marks.set(s, wm.cursor); return s }, events: async () => [], switchWs: async () => {} }
  const exports = {}
  runInNewContext(code, {
    exports, require: (id) => {
      if (id === "./api") return { api, setPageWorkspace: (ws) => { pageWs.push(ws) }, cursorOf: (s) => marks.get(s) ?? null, stream: (event, open, cursor) => { onEvent = event; onOpen = open; cursorFn = cursor; opens++; return () => {} }, Unauthenticated: class extends Error {} }
      if (id === "./event-pages") return pages
      if (id === "./live-utils") return live
      if (id === "./decision-notes") return notes
      if (id === "./feed-order") return feed
      if (id === "./i18n/format") return format
      if (id === "./i18n") return { __esModule: true, ...i18n }
      if (id === "./bus") return { cue: (x) => cues.push(x) }
      if (id === "sonner") return { toast: (x) => notices.push(x) }
      if (id === "react") return {}
      if (id === "./api/yolo") return { yoloApi: { status: async () => null, actions: async () => ({ windows: [] }) } }
      throw new Error(id)
    }, window: {}, document: { querySelector: () => null }, localStorage: { getItem: () => "1" }, location: {},
    setTimeout: (f, ms) => { const timer = { f, ms }; timers.push(timer); return timer }, clearTimeout: (timer) => { if (timer) timer.cancelled = true },
    requestAnimationFrame: (f) => { timers.push({ f, ms: 0 }); return 1 }, crypto: { randomUUID: () => "client-1" },
  })
  return { store: exports, api, state, cues, notices, pageWs, event: (e) => onEvent(e), reconnect: (resumed) => onOpen(resumed), wm, streamCursor: () => cursorFn?.(), opens: () => opens, tick: async (ms) => {
    const due = timers.splice(0); for (const t of due) { if (t.cancelled) continue; if (t.ms <= ms) t.f(); else timers.push(t) } await flush()
  } }
}

test("stream paint preserves unrelated sessions, bots, items and thread references", async () => {
  const h = harness(), s = h.store
  s.start(); await flush(); await s.loadEvents("a"); await s.loadEvents("b")
  const before = { b: s.st.S.b, bots: s.st.bots, items: s.st.items, thread: s.st.thread }
  h.event({ type: "event", session_id: "a", seq: 1, kind: "text", data: { role: "assistant", text: "hello" } })
  await h.tick(80)
  assert.equal(s.st.S.a.ev[0][1], "hello")
  assert.equal(s.st.S.b, before.b); assert.equal(s.st.bots, before.bots); assert.equal(s.st.items, before.items); assert.equal(s.st.thread, before.thread)
})

test("pages paint before EOF; SSE during a load is caught up without duplicates", async () => {
  const h = harness(), s = h.store, eof = defer()
  s.start(); await flush()
  const cursors = []; let calls = 0
  h.api.events = async (_sid, after) => { cursors.push(after); calls++; return calls === 1 ? [{ seq: 1, kind: "user", data: { text: "first" } }] : calls === 2 ? eof.promise : calls === 3 ? [{ seq: 2, kind: "text", data: { role: "assistant", text: "reply" } }] : [] }
  const loading = s.loadEvents("a"); await flush()
  assert.equal(s.st.S.a.loaded, true); assert.equal(s.st.S.a.ev[0][1], "first")
  h.event({ type: "event", session_id: "a", seq: 2, kind: "text", data: { role: "assistant", text: "reply" } })
  eof.resolve([]); await loading; await flush()
  assert.deepEqual(cursors, [0, 1, 1, 2]); assert.equal(s.st.S.a.ev.length, 2)
})

test("reconnect and overflow notify modules, reload details and catch up transcripts", async () => {
  const h = harness(), s = h.store
  let details = 0, events = 0, resyncs = 0
  h.api.item = async () => { details++; return {} }
  s.start(); await flush(); await s.loadItem("loaded"); await s.loadEvents("a")
  h.api.events = async () => { events++; return [] }
  s.hubSubs.add((e) => { if (e.type === "resync") resyncs++ })
  h.reconnect(); await flush()
  h.event({ type: "workspace", id: "ws" }); await flush()
  assert.equal(resyncs, 2); assert.equal(details, 3); assert.equal(events, 2)
})

test("resync marks an in-flight detail for a follow-up request", async () => {
  const h = harness(), s = h.store, first = defer(); let calls = 0
  s.start(); await flush()
  h.api.item = async () => ++calls === 1 ? first.promise : {}
  const loading = s.loadItem("loaded"); h.reconnect(); await flush()
  first.resolve({}); await loading; await flush()
  assert.equal(calls, 2)
})

/* C1: count what a reconnect refetches */
function counted(h) {
  const c = { states: 0, items: [], after: [], resyncs: 0 }
  const state = h.api.state; h.api.state = () => { c.states++; return state() }
  h.api.item = async (id) => { c.items.push(id); return {} }
  h.store.hubSubs.add((e) => { if (e.type === "resync") c.resyncs++ })
  return c
}

test("a resumed reconnect refetches only what the replay touched, and the gap's transcript once", async () => {
  const h = harness(), s = h.store, c = counted(h)
  h.wm.cursor = "E:5"
  s.start(); await flush()
  assert.equal(h.opens(), 1); assert.equal(h.streamCursor(), "E:5"); assert.equal(c.states, 1) // the stream opens after the state it continues
  const log = [1]
  h.api.events = async (_sid, after) => { c.after.push(after); return log.filter((seq) => seq > after).map((seq) => ({ seq, kind: "text", data: { role: "assistant", text: "ABC"[seq - 1] } })) }
  await s.loadItem("A"); await s.loadItem("B"); await s.loadEvents("a")
  c.items.length = 0; c.after.length = 0
  log.push(2, 3) // written while the page was away
  h.reconnect(true); await flush()
  assert.equal(c.resyncs, 0); assert.equal(c.states, 1)
  h.event({ type: "item", id: "A" }) // replayed
  h.event({ type: "stream.gap" })
  h.event({ type: "event", session_id: "a", seq: 3, kind: "text", data: { role: "assistant", text: "C" } }) // live, before the catch-up answers
  await h.tick(1000); await h.tick(1000)
  assert.deepEqual(c.items, ["A"]); assert.equal(c.states, 2); assert.equal(c.resyncs, 0)
  assert.equal(c.after[0], 1) // from before the gap, not from the live frame (then the next page, and the frame seen during the load)
  assert.ok(c.after.every((x) => x >= 1), String(c.after))
  await h.tick(80); assert.equal(s.st.S.a.ev.length, 1); assert.equal(s.st.S.a.ev[0][1], "ABC") // every missed chunk once
})

test("a stream reset resyncs everything once", async () => {
  const h = harness(), s = h.store, c = counted(h)
  h.wm.cursor = "E:5"
  s.start(); await flush(); await s.loadItem("A"); await s.loadItem("B")
  c.items.length = 0
  h.reconnect(true); h.event({ type: "stream.reset" }); await flush()
  assert.equal(c.resyncs, 1); assert.equal(c.states, 2); assert.deepEqual(c.items.sort(), ["A", "B"])
})

test("without a cursor every open resyncs, and the state it loaded is the cursor for the next reconnect", async () => {
  const h = harness(), s = h.store, c = counted(h)
  s.start(); await flush()
  assert.equal(h.streamCursor(), null)
  h.wm.cursor = "E:9"
  h.reconnect(false); await flush()
  assert.equal(c.resyncs, 1); assert.equal(h.streamCursor(), "E:9")
})

test("a workspace switch opens a new stream from the new workspace's state", async () => {
  const h = harness(), s = h.store
  h.wm.cursor = "E:5"
  s.start(); await flush()
  h.wm.cursor = "E:12"
  await s.switchWorkspace("other")
  assert.equal(h.opens(), 2); assert.equal(h.streamCursor(), "E:12")
})

test("old workspace event pages cannot populate the new workspace", async () => {
  const h = harness(), s = h.store, page = defer()
  s.start(); await flush(); h.api.events = async () => page.promise
  const loading = s.loadEvents("a"); await s.switchWorkspace("other")
  page.resolve([{ seq: 1, kind: "user", data: { text: "old workspace" } }]); await loading
  assert.equal(s.st.S.a.loaded, false)
})

test("session and decision actions guard duplicates and report no early success", async () => {
  const h = harness(), s = h.store, send = defer(), decision = defer(); let sends = 0, decisions = 0
  h.state.decisions = [{ id: "d", kind: "accept", session_id: "a", subject_type: "item", question: "ok?", options: [], created: 1 }]
  s.start(); await flush()
  h.api.sessionSay = () => { sends++; return send.promise }
  const sending = s.sessSend("a", "hello", "排队"); assert.equal(s.st.pendingActions.has("session:a"), true)
  assert.equal(await s.sessSend("a", "hello", "排队"), false); assert.equal(sends, 1); assert.equal(h.notices.length, 0)
  send.resolve({}); await sending; assert.equal(s.st.pendingActions.size, 0)
  h.api.decide = () => { decisions++; return decision.promise }
  const resolving = s.decide("", "pass", undefined, "d"); await s.decide("", "pass", undefined, "d")
  assert.equal(decisions, 1); assert.equal(h.cues.includes("success"), false); assert.equal(s.st.pendingActions.has("decision:d"), true)
  decision.reject(new Error("denied")); await resolving
  assert.equal(s.st.pendingActions.size, 0); assert.equal(h.cues.includes("success"), false)
})

test("overflow freezes the cursor before newer SSE frames arrive during state refresh", async () => {
  const h = harness(), s = h.store, state = defer()
  s.start(); await flush(); await s.loadEvents("a")
  const initialState = h.api.state
  h.api.state = () => state.promise
  h.event({ type: "workspace", id: "ws" })
  h.event({ type: "event", session_id: "a", seq: 3, kind: "text", data: { role: "assistant", text: "C" } })
  const cursors = []
  h.api.events = async (_sid, after) => { cursors.push(after); return after === 0 ? [1, 2, 3].map((seq) => ({ seq, kind: "text", data: { role: "assistant", text: "ABC"[seq - 1] } })) : [] }
  state.resolve(await initialState()); await flush(); await h.tick(80)
  assert.deepEqual(cursors, [0, 3]); assert.equal(s.st.S.a.ev[0][1], "ABC")
})

test("failed overflow catch-up retains the pre-gap cursor for retry", async () => {
  const h = harness(), s = h.store, state = defer()
  s.start(); await flush(); await s.loadEvents("a")
  const initialState = h.api.state; h.api.state = () => state.promise
  h.reconnect()
  h.event({ type: "event", session_id: "a", seq: 3, kind: "text", data: { role: "assistant", text: "C" } })
  const cursors = []; let fail = true
  h.api.events = async (_sid, after) => { cursors.push(after); if (fail) { fail = false; throw new Error("offline") } return after === 0 ? [1, 2, 3].map((seq) => ({ seq, kind: "text", data: { role: "assistant", text: "ABC"[seq - 1] } })) : [] }
  state.resolve(await initialState()); await flush(); await h.tick(2000)
  assert.deepEqual(cursors, [0, 0, 3]); assert.equal(s.st.S.a.ev[0][1], "ABC")
})

test("resync transcript fetches never exceed three concurrent requests", async () => {
  const h = harness(), s = h.store
  h.state.sessions = Array.from({ length: 9 }, (_, n) => session(String(n)))
  s.start(); await flush(); for (const sess of h.state.sessions) await s.loadEvents(sess.id)
  let active = 0, peak = 0, calls = 0
  h.api.events = async () => { calls++; peak = Math.max(peak, ++active); await new Promise((r) => setImmediate(r)); active--; return [] }
  h.reconnect()
  for (let i = 0; i < 10; i++) await new Promise((r) => setImmediate(r))
  assert.equal(calls, 9); assert.equal(peak, 3)
})

test("the page names its workspace from the state it shows, and the one it switched to", async () => {
  const h = harness(), s = h.store
  s.start(); await flush()
  assert.deepEqual(h.pageWs, ["ws"])
  h.state.workspace = { id: "ws2", item_prefix: "WB" }
  await s.switchWorkspace("ws2"); await flush()
  assert.equal(h.pageWs.at(-1), "ws2")
})

test("acceptance modes round-trip through their Hub keys in en", async () => {
  await i18n.default.changeLanguage("en")
  try {
    const h = harness(), s = h.store, sent = []
    h.state.items = [{ id: "ST-1", title: "x", status: "wait", accept_mode: "auto", deps: [], source: [], tags: [], plan: [], sessionIds: [], timeline: [], created: 1, updated: 2 }]
    h.api.patchItem = async (id, p) => { sent.push([id, p]); return {} }
    s.start(); await flush()
    const i = s.st.items.find((x) => x.id === "ST-1")
    assert.equal(i.acc, s.ACC.auto); assert.equal(s.accKey(i.acc), "auto")
    assert.equal(s.ACC.manual, "You review")
    // the item selector offers the Hub keys and hands the chosen key to setAcc
    for (const k of ["inherit", "manual", "auto"]) { assert.equal(s.accKey(s.ACC[k]), k); s.setAcc("ST-1", k) }
    await flush()
    assert.deepEqual(sent.map(([, p]) => p.accept_mode), ["inherit", "manual", "auto"])
  } finally { await i18n.default.changeLanguage("zh-CN") }
})

test("an approved release that was not released says so by its outcome in en, not by its zh-CN text", async () => {
  const s = harness().store
  await i18n.default.changeLanguage("en")
  try {
    const d = (result) => ({ id: "d1", kind: "approve", subject_type: "release", status: "resolved", result: { approved: true, ...result } })
    const not = s.relOf(d({ outcome: "not_released" }))
    assert.equal(not.tone, "fail"); assert.equal(not.outcome, "not_released"); assert.doesNotMatch(not.text, /[一-鿿]/)
    assert.equal(s.relOf(d({ outcome: "not_released", reason: "no changes" })).outcome, "not_released")
    assert.equal(s.relOf(d({ outcome: "failed" })).outcome, "failed")
  } finally { await i18n.default.changeLanguage("zh-CN") }
})
