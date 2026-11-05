import assert from "node:assert/strict"
import { test } from "node:test"
import { resumableStream, STALE_MS } from "../src/stream-cursor.ts"

/* a fake EventSource per open, fake timers and clock, and window/document targets */
function rig({ cursor = () => null, hidden = false } = {}) {
  const sources = [], data = [], opens = [], timers = []
  let now = 0
  const target = () => { const ls = {}; return { ls, addEventListener: (t, f) => { (ls[t] ??= new Set()).add(f) }, removeEventListener: (t, f) => ls[t]?.delete(f), fire: (t) => ls[t]?.forEach((f) => f()) } }
  const win = target(), doc = Object.assign(target(), { visibilityState: hidden ? "hidden" : "visible" })
  const s = resumableStream({
    url: (after) => "/api/stream" + (after ? "?after=" + after : ""),
    source: (url) => {
      const ls = {}
      const src = { url, readyState: 0, closed: false, onopen: null, onmessage: null, onerror: null, addEventListener: (t, f) => { (ls[t] ??= []).push(f) }, close() { this.closed = true; this.readyState = 2 } }
      src.open = () => { src.readyState = 1; src.onopen?.({}) }
      src.send = (d, id = "") => src.onmessage?.({ data: d, lastEventId: id })
      src.ping = () => ls.ping?.forEach((f) => f())
      src.drop = () => { src.readyState = 2; src.onerror?.({}) }
      sources.push(src); return src
    },
    on: (d) => data.push(d), onOpen: (resumed) => opens.push(resumed), cursor,
    setTimeout: (f, ms) => { const t = { f, ms, at: now + ms }; timers.push(t); return t }, clearTimeout: (t) => { if (t) t.cancelled = true },
    now: () => now, win, doc,
  })
  const advance = (ms) => { now += ms; for (const t of timers.splice(0)) { if (t.cancelled) continue; if (t.at <= now) t.f(); else timers.push(t) } }
  return { s, sources, data, opens, timers, win, doc, advance, last: () => sources.at(-1) }
}

test("the first open resumes after the state's watermark; without one it opens as before", () => {
  const r = rig({ cursor: () => "E:5" })
  assert.equal(r.last().url, "/api/stream?after=E:5")
  r.last().open(); assert.deepEqual(r.opens, [true])
  const legacy = rig()
  assert.equal(legacy.last().url, "/api/stream")
  legacy.last().open(); assert.deepEqual(legacy.opens, [false])
})

test("a reconnect carries the last frame id; pings and frames without an id don't move it", () => {
  const r = rig({ cursor: () => "E:5" })
  r.last().open()
  r.last().send('{"type":"item","id":"A"}', "E:7")
  r.last().send('{"type":"event"}') // a transcript chunk: no id
  r.last().ping()
  r.last().drop()
  assert.equal(r.sources[0].closed, true)
  r.advance(2000)
  assert.equal(r.sources.length, 2)
  assert.equal(r.last().url, "/api/stream?after=E:7")
  r.last().open(); assert.deepEqual(r.opens, [true, true])
  assert.deepEqual(r.data, ['{"type":"item","id":"A"}', '{"type":"event"}'])
  assert.equal(r.s.cursor(), "E:7")
})

test("a reset moves the cursor to the Hub's head, so the next reconnect does not reset again", () => {
  const r = rig({ cursor: () => "OLD:9" })
  r.last().open(); r.last().send('{"type":"stream.reset"}', "NEW:3")
  r.last().drop(); r.advance(2000)
  assert.equal(r.last().url, "/api/stream?after=NEW:3")
})

test("coming back to the page reconnects at once instead of waiting out the backoff", () => {
  const r = rig({ cursor: () => "E:1" })
  r.last().open(); r.last().drop()
  r.advance(2000); r.last().drop(); r.advance(4000); r.last().drop()
  assert.equal(r.sources.length, 3)
  r.advance(1000); assert.equal(r.sources.length, 3) // the next try is 8 s away
  r.doc.fire("visibilitychange")
  assert.equal(r.sources.length, 4)
  r.advance(60_000); assert.equal(r.sources.length, 4) // the pending backoff was cancelled
  r.last().drop(); r.advance(2000); assert.equal(r.sources.length, 5) // and the backoff restarted from 2 s
  r.win.fire("online"); r.win.fire("pageshow") // a connection is being made: nothing to do
  assert.equal(r.sources.length, 5)
})

test("an open stream gone silent past two pings is reopened on foreground; a live one is left alone", () => {
  const r = rig({ cursor: () => "E:1" })
  r.last().open()
  r.advance(20_000); r.last().ping(); r.advance(20_000)
  r.doc.fire("visibilitychange"); assert.equal(r.sources.length, 1)
  r.advance(STALE_MS)
  r.win.fire("pageshow")
  assert.equal(r.sources.length, 2); assert.equal(r.sources[0].closed, true)
  r.sources[0].onerror?.({}) // the old socket's late error must not schedule another open
  r.advance(60_000); assert.equal(r.sources.length, 2)
})

test("a hidden page does not reconnect early, and close stops everything", () => {
  const r = rig({ cursor: () => "E:1", hidden: true })
  r.last().open(); r.last().drop()
  r.doc.fire("visibilitychange"); assert.equal(r.sources.length, 1)
  r.s.close()
  r.advance(60_000); r.win.fire("online")
  assert.equal(r.sources.length, 1)
})
