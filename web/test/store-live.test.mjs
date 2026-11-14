import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "typescript"
import * as pages from "../src/event-pages.ts"
import * as live from "../src/live-utils.ts"
import * as notes from "../src/decision-notes.ts"
import * as feed from "../src/feed-order.ts"
import * as format from "../src/i18n/format.ts"
import * as i18n from "../src/i18n/index.ts"
import * as composerDraft from "../src/composer-draft.ts"

const code = ts.transpileModule(readFileSync(new URL("../src/store.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve() }
const defer = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b }); return { promise, resolve, reject } }
const session = (id) => ({ id, no: 1, item_id: null, status: "running", created: 1, updated: 2, queued: [], harness: "codex" })
function harness(local = { getItem: () => "1" }) {
  const state = { workspace: { id: "ws", item_prefix: "ST" }, workspaces: [], items: [], bots: [], allBots: [], projects: [], sessions: [session("a"), session("b")], decisions: [], recent: [], thread: [], tags: [], tagUses: {}, settings: {}, devices: [], runtimes: [] }
  let onEvent, onOpen, cursorFn, opens = 0, closes = 0
  const marks = new WeakMap(), wm = { cursor: null }
  const timers = [], cues = [], notices = [], pageWs = [], looks = []
  const api = { state: async () => { const s = structuredClone(state); if (wm.cursor) marks.set(s, wm.cursor); return s }, events: async () => [], switchWs: async () => {} }
  const exports = {}
  runInNewContext(code, {
    exports, require: (id) => {
      if (id === "./api") return { api, setPageWorkspace: (ws) => { pageWs.push(ws) }, cursorOf: (s) => marks.get(s) ?? null, stream: (event, open, cursor) => { onEvent = event; onOpen = open; cursorFn = cursor; opens++; return () => { closes++ } }, Unauthenticated: class extends Error {} }
      if (id === "./event-pages") return pages
      if (id === "./live-utils") return live
      if (id === "./decision-notes") return notes
      if (id === "./feed-order") return feed
      if (id === "./i18n/format") return format
      if (id === "./i18n") return { __esModule: true, ...i18n }
      if (id === "./composer-draft") return composerDraft
      if (id === "./lib/motion") return { applyLook: (x) => looks.push({ fontSize: x.fontSize, reduceMotion: x.reduceMotion }) }
      if (id === "./bus") return { cue: (x) => cues.push(x) }
      if (id === "./components/ui/toaster") return { toast: (x) => notices.push(x) }
      if (id === "react") return {}
      if (id === "./api/yolo") return { yoloApi: { status: async () => null, actions: async () => ({ windows: [] }) } }
      throw new Error(id)
    }, window: {}, document: { querySelector: () => null }, localStorage: local, location: {},
    setTimeout: (f, ms) => { const timer = { f, ms }; timers.push(timer); return timer }, clearTimeout: (timer) => { if (timer) timer.cancelled = true },
    requestAnimationFrame: (f) => { timers.push({ f, ms: 0 }); return 1 }, crypto: { randomUUID: () => "client-1" },
  })
  return { store: exports, api, state, cues, notices, pageWs, looks, event: (e) => onEvent(e), reconnect: (resumed) => onOpen(resumed), wm, streamCursor: () => cursorFn?.(), opens: () => opens, closes: () => closes, cursorFn: () => cursorFn, tick: async (ms) => {
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

test("capacity-only refreshes update wait counts and actual Node reasons without new transcript events", async () => {
  const h = harness(), s = h.store, row = h.state.sessions[0]
  row.room = { running: 32, max: 32, waiting: 2, reason: "capacity" }
  s.start(); await flush(); await s.loadEvents("a")
  const events = s.st.S.a.ev
  assert.equal(s.st.S.a.room.running, 32)
  row.room = { running: 33, max: 16, waiting: 3, reason: "capacity" }
  await s.refresh()
  assert.equal(s.st.S.a.room.max, 16)
  assert.equal(s.st.S.a.room.waiting, 3)
  assert.equal(s.st.S.a.ev, events)
  row.room = { running: 0, max: 64, waiting: 1, reason: "node", detail: "journal storage full" }
  await s.refresh()
  assert.equal(s.st.S.a.room.reason, "node")
  assert.equal(s.st.S.a.room.detail, "journal storage full")
  row.room = null
  await s.refresh()
  assert.equal(s.st.S.a.room, null)
  assert.equal(s.st.S.a.ev, events)
  row.status = "ended"; row.room = { running: 32, max: 32, waiting: 1, reason: "capacity" }
  await s.refresh()
  assert.equal(s.st.S.a.room, null, "settled sessions cannot retain a stale wait")
})

test("each state refresh applies the Hub's type size and reduced-motion settings to the page", async () => {
  const h = harness(), s = h.store
  h.state.settings = { fontSize: "大", reduceMotion: "开" }
  s.start(); await flush()
  assert.deepEqual(h.looks.at(-1), { fontSize: "大", reduceMotion: "开" })
  h.state.settings = { fontSize: "标准", reduceMotion: "关" }
  await s.refresh()
  assert.deepEqual(h.looks.at(-1), { fontSize: "标准", reduceMotion: "关" })
})

const chatMessage = (id, author, origin) => ({ id, author, text: id, created: 1, item_id: null, meta: origin ? { origin } : {} })
const draft = (h, mid, text) => h.event({ type: "steve.draft", ws: "ws", mid, thought: "", text })

test("chat drafts paint successive chunks before any stored reply arrives", async () => {
  const h = harness(), s = h.store, c = counted(h)
  h.state.thread = [chatMessage("question", "me")]
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true, phase: "answer" })
  const version = s.st.ver
  draft(h, "question", "第一段"); await h.tick(0)
  assert.equal(s.st.draft.text, "第一段"); assert.ok(s.st.ver > version)
  draft(h, "question", "第一段，第二段"); await h.tick(0)
  assert.equal(s.st.draft.text, "第一段，第二段")
  assert.equal(s.st.thread.filter((m) => m.k === "thinking").length, 1)
  assert.equal(s.st.thread.filter((m) => m.k === "steve").length, 0)
  assert.equal(c.states, 1)
})

test("unrelated Steve messages keep the live draft; only its own reply replaces it", async () => {
  const h = harness(), s = h.store
  h.state.thread = [chatMessage("question", "me")]
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "question", "仍在生成")
  h.state.thread.push(chatMessage("item-progress", "steve", "other-question"))
  await s.refresh()
  assert.equal(s.st.draft.text, "仍在生成"); assert.equal(s.st.drafted, "")
  // The original question may have moved outside /api/state's message window.
  h.state.thread = [chatMessage("item-progress", "steve", "other-question"), chatMessage("answer", "steve", "question")]
  await s.refresh()
  assert.equal(s.st.draft, null); assert.equal(s.st.drafted, "answer")
  assert.equal(s.st.thread.filter((m) => m.id === "answer").length, 1)
})

test("rerunning a question does not replace its new draft with the previous reply", async () => {
  const h = harness(), s = h.store
  h.state.thread = [chatMessage("question", "me"), chatMessage("previous-answer", "steve", "question")]
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "question", "重新生成")
  await s.refresh()
  assert.equal(s.st.draft.text, "重新生成"); assert.equal(s.st.drafted, "")
  h.state.thread.push(chatMessage("new-answer", "steve", "question"))
  h.event({ type: "steve.thinking", on: false }); await s.refresh()
  assert.equal(s.st.draft, null); assert.equal(s.st.drafted, "new-answer")
})

test("a state request begun while idle cannot clear a turn that started during the request", async () => {
  const h = harness(), s = h.store, response = defer()
  s.start(); await flush()
  const old = await h.api.state()
  h.api.state = () => response.promise
  const loading = s.refresh()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "new-question", "新回复")
  response.resolve(old); await loading
  assert.equal(s.st.draft.text, "新回复"); assert.equal(s.st.thinking, true)
})

test("a stopped turn's refresh cannot clear the next turn's draft", async () => {
  const h = harness(), s = h.store, response = defer()
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "first-question", "第一条")
  h.event({ type: "steve.thinking", on: false })
  const old = await h.api.state()
  h.api.state = () => response.promise
  const loading = s.refresh()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "next-question", "第二条")
  response.resolve(old); await loading
  assert.equal(s.st.draft.text, "第二条"); assert.equal(s.st.thinking, true)
})

test("a settled turn without a stored reply clears its draft after a fresh idle snapshot", async () => {
  const h = harness(), s = h.store
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "question", "尚未保存")
  h.event({ type: "steve.thinking", on: false }); await s.refresh()
  assert.equal(s.st.draft, null)
  assert.equal(s.st.thread.some((m) => m.k === "thinking"), false)
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

test("without a cursor every open resyncs, and the stream is reopened from the state that resync loaded", async () => {
  const h = harness(), s = h.store, c = counted(h)
  s.start(); await flush()
  assert.equal(h.streamCursor(), null)
  h.reconnect(false); await flush() // an old Hub: no watermark, the stream stays as it is
  assert.equal(c.resyncs, 1); assert.equal(h.opens(), 1); assert.equal(h.streamCursor(), null)
  h.wm.cursor = "E:9"
  h.reconnect(false); await flush()
  assert.equal(c.resyncs, 2); assert.equal(h.opens(), 2); assert.equal(h.closes(), 1); assert.equal(h.streamCursor(), "E:9")
})

test("a research tool call is on the live line while it runs, and a step once it ends", async () => {
  const h = harness(), s = h.store
  h.state.thread = [chatMessage("question", "me")]
  s.start(); await flush()
  const tool = (run, extra) => h.event({ type: "steve.tool", ws: "ws", mid: "question", run, name: "Bash", brief: "du -sh ~", ok: true, ...extra })
  const live = () => JSON.parse(JSON.stringify({ run: s.st.live.run, now: s.st.live.now ?? null, tools: s.st.live.tools }))
  tool("tr_1", { kind: "exec", running: true }); await h.tick(0)
  assert.deepEqual(live(), { run: "tr_1", now: { name: "Bash", brief: "du -sh ~", kind: "exec" }, tools: [] })
  assert.equal(s.st.thread.filter((m) => m.k === "thinking").length, 1)
  tool("tr_1", { kind: "exec" }); await h.tick(0)
  assert.deepEqual(live(), { run: "tr_1", now: null, tools: [{ name: "Bash", brief: "du -sh ~", ok: true, kind: "exec" }] })
  /* a Hub that says no category, and the next run starting over */
  tool("tr_1", {}); await h.tick(0)
  assert.equal(live().tools.length, 2); assert.equal(live().tools[1].kind, undefined)
  tool("tr_2", { running: true }); await h.tick(0)
  assert.deepEqual(live(), { run: "tr_2", now: { name: "Bash", brief: "du -sh ~" }, tools: [] })
})

test("a stream opened without a cursor never resumes from a later watermark, so its live frames are not replayed", async () => {
  const h = harness(), s = h.store
  s.start(); await flush()
  const legacyCursor = h.cursorFn() // the legacy stream's own cursor
  h.wm.cursor = "E:9"
  h.reconnect(false) // the legacy open: its resync loads E:9
  h.event({ type: "steve.tool", ws: "ws", mid: "m1", run: "r1", name: "search", brief: "q", ok: true }) // live, no frame id
  await flush()
  assert.equal(s.st.live.tools.length, 1)
  // before: this stream's next reconnect sent ?after=E:9 and the Hub replayed the tool row (and everything else) again
  assert.equal(legacyCursor(), null); assert.equal(h.closes(), 1)
  assert.equal(h.opens(), 2); assert.equal(h.streamCursor(), "E:9") // reopened from the watermark: its frames carry ids
  h.reconnect(true); await flush() // a resumed open of the new stream resyncs nothing and reopens nothing
  assert.equal(h.opens(), 2); assert.equal(s.st.live.tools.length, 1)
})

test("a stopped store does not reopen the stream after a legacy resync", async () => {
  const h = harness(), s = h.store
  const stop = s.start(); await flush()
  h.wm.cursor = "E:9"
  h.reconnect(false); stop(); await flush()
  assert.equal(h.opens(), 1)
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

test("sends carry their idempotency key: the composer's on every retry, a fresh one otherwise", async () => {
  const h = harness(), s = h.store
  s.start(); await flush()
  const said = [], made = []
  h.api.say = async (_t, _f, _m, to) => { said.push(to.client_id); throw new Error("lost answer") }
  h.api.newItem = async (p) => { made.push(p.client_id); return { id: "ST-1" } }
  assert.equal(await s.chatSend("你好", [], [], [], "draft-1"), false)
  assert.equal(await s.chatSend("你好", [], [], [], "draft-1"), false)
  await s.chatSend("重来")
  assert.equal(said[0], "draft-1"); assert.equal(said[1], "draft-1")
  assert.ok(said[2] && said[2] !== "draft-1")
  await s.newItem("目标", undefined, undefined, "start", "form-1")
  await s.newItem("目标")
  assert.equal(made[0], "form-1"); assert.ok(made[1] && made[1] !== "form-1")
})

/* a browser's localStorage that starts empty */
const browser = () => { const kept = new Map(); return { kept, getItem: (k) => kept.get(k) ?? null, setItem: (k, v) => { kept.set(k, v) }, removeItem: (k) => { kept.delete(k) } } }

test("the owner's tour ends in the first meeting, which the welcome's skip ends", async () => {
  const local = browser(), h = harness(local), s = h.store, calls = []
  h.state.owner = true; h.state.sessions = []
  h.api.onboardStart = async () => { calls.push("start"); h.state.settings = { onboard: "pending", onboardFrom: "hello" }; h.state.thread = [{ id: "hello", author: "steve", text: "你好，我是 Steve。", created: 1, item_id: null, meta: { onboard: true } }]; return { message: "hello" } }
  h.api.onboardSkip = async () => { calls.push("skip"); h.state.settings = { onboard: "done", onboardFrom: "hello" } }
  s.start(); await flush()
  assert.equal(s.st.onboard, 0); assert.equal(s.st.owner, true); assert.equal(s.meeting(), false)
  await s.meet()
  assert.deepEqual(calls, ["start"]); assert.equal(s.st.onboard, -1); assert.equal(local.kept.get("steve-onboarded"), "1")
  assert.equal(s.meeting(), true); assert.equal(s.st.thread.find((m) => m.id === "hello").meet, true)
  await s.endMeeting()
  assert.deepEqual(calls, ["start", "skip"]); assert.equal(s.meeting(), false); assert.equal(h.notices.length, 1)
})

test("the Hub remembers the owner's answer for every browser; anyone else's tour is this browser's", async () => {
  /* the owner met Steve or said no: a new browser on the still empty instance shows no tour */
  const met = harness(browser())
  met.state.owner = true; met.state.sessions = []; met.state.settings = { onboard: "done" }
  met.store.start(); await flush()
  assert.equal(met.store.st.onboard, -1)
  /* the owner's skip is the Hub's to remember */
  const local = browser(), h = harness(local), s = h.store, calls = []
  h.state.owner = true; h.state.sessions = []
  h.api.onboardSkip = async () => { calls.push("skip"); h.state.settings = { onboard: "done" } }
  s.start(); await flush()
  assert.equal(s.st.onboard, 0)
  s.skipOnboarding(); await flush()
  assert.deepEqual(calls, ["skip"]); assert.equal(s.st.onboard, -1); assert.equal(s.st.settings.onboard, "done")
  /* a member has no first meeting: the tour and its skip stay in the browser */
  const theirs = browser(), m = harness(theirs)
  m.state.sessions = []; m.api.onboardSkip = async () => { calls.push("member") }
  m.store.start(); await flush()
  assert.equal(m.store.st.onboard, 0); assert.equal(m.store.st.owner, false)
  m.store.skipOnboarding(); await flush()
  assert.deepEqual(calls, ["skip"]); assert.equal(m.store.st.onboard, -1); assert.equal(theirs.kept.get("steve-onboarded"), "1")
})

test("defaultHarness, permModes, defaultPermMode and makeBot adapt across heterogeneous runtimes", async () => {
  const h = harness(), s = h.store, created = []
  h.api.newBot = async (p) => { created.push(p); return { id: "bot-2" } }
  h.state.runtimes = [
    { id: "claude-code", name: "Claude Code", ok: false, modes: ["plan", "ask", "accept_edits", "auto", "full_access"] },
    { id: "cursor", name: "Cursor", ok: true, modes: ["plan", "ask"] },
    { id: "opencode", name: "OpenCode", ok: true, modes: ["plan", "auto"] },
    { id: "custom-empty", name: "Custom Empty", ok: true, modes: [] },
    { id: "custom-exotic", name: "Custom Exotic", ok: true, modes: ["weird_mode"] },
  ]
  h.state.bots = [{ id: "main", name: "Steve", role: "main", main: true, harness: "opencode", perm_mode: "auto", accept_mode: "inherit", proactivity: 1, workspaces: ["ws"] }]
  s.start(); await flush()

  // Main bot's harness is ready -> defaultHarness picks main bot's harness ("opencode")
  assert.equal(s.defaultHarness(), "opencode")

  // If main bot's harness is unavailable -> falls back to first ready runtime ("cursor")
  h.state.bots[0].harness = "claude-code"
  await s.refresh()
  assert.equal(s.defaultHarness(), "cursor")

  // permModes maps Hub mode names to store keys and never returns empty
  assert.deepEqual([...s.permModes("cursor")], ["plan", "default"])
  assert.deepEqual([...s.permModes("opencode")], ["plan", "auto"])
  assert.equal(s.permModes("custom-empty").length, 5)
  assert.deepEqual([...s.permModes("custom-exotic")], ["default"])

  // defaultPermMode picks a compatible mode in priority order (default -> auto -> acceptEdits -> plan -> bypassPermissions)
  assert.equal(s.defaultPermMode("cursor"), "default")
  assert.equal(s.defaultPermMode("opencode"), "auto")
  assert.equal(s.defaultPermMode("custom-exotic"), "default")

  // makeBot sends a compatible perm_mode for the selected harness
  const id1 = await s.makeBot("Worker A", "coder", "opencode")
  assert.equal(id1, "bot-2")
  assert.equal(created[0].harness, "opencode")
  assert.equal(created[0].perm_mode, "auto")

  const id2 = await s.makeBot("Worker B", "coder")
  assert.equal(id2, "bot-2")
  assert.equal(created[1].harness, "cursor")
  assert.equal(created[1].perm_mode, "default")
})
