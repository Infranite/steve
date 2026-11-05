import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "../node_modules/typescript/lib/typescript.js"
import * as pages from "../src/event-pages.ts"
import * as live from "../src/live-utils.ts"
import * as notes from "../src/decision-notes.ts"
import * as feed from "../src/feed-order.ts"

const code = ts.transpileModule(readFileSync(new URL("../src/store.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve() }
const defer = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b }); return { promise, resolve, reject } }
const session = (id) => ({ id, no: 1, item_id: null, status: "running", created: 1, updated: 2, queued: [], harness: "codex" })
function harness() {
  const state = { workspace: { id: "ws", item_prefix: "ST" }, workspaces: [], items: [], bots: [], allBots: [], projects: [], sessions: [session("a"), session("b")], decisions: [], recent: [], thread: [], tags: [], tagUses: {}, settings: {}, devices: [], runtimes: [] }
  let onEvent, onOpen
  const timers = [], cues = [], notices = [], pageWs = []
  const api = { state: async () => structuredClone(state), events: async () => [], switchWs: async () => {} }
  const exports = {}
  runInNewContext(code, {
    exports, require: (id) => {
      if (id === "./api") return { api, setPageWorkspace: (ws) => { pageWs.push(ws) }, stream: (event, open) => { onEvent = event; onOpen = open; return () => {} }, Unauthenticated: class extends Error {} }
      if (id === "./event-pages") return pages
      if (id === "./live-utils") return live
      if (id === "./decision-notes") return notes
      if (id === "./feed-order") return feed
      if (id === "./bus") return { cue: (x) => cues.push(x) }
      if (id === "sonner") return { toast: (x) => notices.push(x) }
      if (id === "react") return {}
      if (id === "./api/yolo") return { yoloApi: { status: async () => null, actions: async () => ({ windows: [] }) } }
      throw new Error(id)
    }, window: {}, document: { querySelector: () => null }, localStorage: { getItem: () => "1" }, location: {},
    setTimeout: (f, ms) => { const timer = { f, ms }; timers.push(timer); return timer }, clearTimeout: (timer) => { if (timer) timer.cancelled = true },
    requestAnimationFrame: (f) => { timers.push({ f, ms: 0 }); return 1 }, crypto: { randomUUID: () => "client-1" },
  })
  return { store: exports, api, state, cues, notices, pageWs, event: (e) => onEvent(e), reconnect: () => onOpen(), tick: async (ms) => {
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
