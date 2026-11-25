import * as decisionText from "../src/i18n/decision.ts"
import { isCancelledError, QueryClient, timeoutManager } from "@tanstack/react-query"
import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "typescript"
import * as live from "../src/live-utils.ts"
import * as notes from "../src/decision-notes.ts"
import * as decisionOptions from "../src/decision-options.ts"
import * as feed from "../src/feed-order.ts"
import * as format from "../src/i18n/format.ts"
import * as i18n from "../src/i18n/index.ts"
import * as composerDraft from "../src/composer-draft.ts"
import * as runtimeStatus from "../src/runtime-status.ts"
import * as itemState from "../src/item-state.ts"

import * as chatProgress from "../src/thread/chat-progress.ts"
import * as tbl from "../src/data/table.ts"
import * as streamCursor from "../src/stream-cursor.ts"

// A cache's five-minute eviction timer should not keep the test process alive.
timeoutManager.setTimeoutProvider({
  setTimeout: (fn, ms) => setTimeout(fn, ms).unref(), clearTimeout,
  setInterval: (fn, ms) => setInterval(fn, ms).unref(), clearInterval,
})

/* data/sync.ts without TanStack Query: the same table functions behind the same names */
function fakeSync(snapshot) {
  const qc = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, gcTime: Infinity, retry: false } } })
  let t = tbl.EMPTY, scope = 0
  const ls = new Set(), catalog = new Set()
  const set = (n, all = true) => { if (n !== t) { t = n; ls.forEach((f) => f()); if (all) catalog.forEach((f) => f()) } }
  let cursor = null
  const open = () => tbl.viewsOf(t, "decision").filter((d) => d.status === "open")
  return {
    decisionsForItem: (id) => open().filter((d) => d.item_id === id),
    decisionsForSession: (id) => open().filter((d) => d.session_id === id),
    applyPatch: (e) => set(tbl.mergePatch(t, e.rev, e.ops), e.ops.some((op) => op.type !== "message")),
    decisionView: (id) => tbl.viewOf(t, "decision", id),
    loadViews: async () => { cursor = snapshot.cursor ?? null; set(tbl.mergeSnapshot(t, snapshot.rev, {
      decision: structuredClone(snapshot.decision), item: structuredClone(snapshot.item), session: structuredClone(snapshot.session),
      message: { views: structuredClone(snapshot.message) },
    })) },
    messageView: (id) => tbl.viewOf(t, "message", id),
    messageViews: () => tbl.viewsOf(t, "message"),
    onCatalog: (f) => { catalog.add(f); return () => catalog.delete(f) },
    itemView: (id) => tbl.viewOf(t, "item", id),
    sessionView: (id) => tbl.viewOf(t, "session", id),
    itemViews: () => tbl.viewsOf(t, "item", "hot:item"),
    sessionViews: () => tbl.viewsOf(t, "session", "hot:session"),
    sessionsForItem: (id) => tbl.viewsOf(t, "session", `session:item:${id}`),
    itemWork: (id) => {
      const live = tbl.viewsOf(t, "session", `session:item:${id}`).filter((s) => s.status !== "ended")
      return { active: live.at(-1), running: live.some((s) => s.status === "running"),
        queued: live.at(-1)?.queued ?? 0, decisions: open().filter((d) => d.item_id === id) }
    },
    table: () => t, qc, scopeKey: () => scope,
    readViews: async () => {},
    readItemSessions: async () => {},
    onViews: (f) => { ls.add(f); return () => ls.delete(f) },
    openDecisions: open,
    resetViews: () => { t = tbl.EMPTY; qc.clear(); scope++ },
    settle: async (answer) => answer, settled: () => [], viewsCursor: () => cursor,
  }
}

const compile = (file) => ts.transpileModule(readFileSync(new URL("../src/" + file, import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
const code = compile("store.ts")
const liveCode = compile("data/live.ts")
const transcriptCode = compile("data/transcript.ts")
const transcriptModelCode = compile("data/transcript-model.ts")
const page = (events = [], next = null) => ({ events, next })
const chunk = (seq, text = "ABC"[seq - 1]) => ({ seq, ts: seq, kind: "text", data: { role: "assistant", text } })
const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve() }
const defer = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b }); return { promise, resolve, reject } }
const session = (id) => ({ id, no: 1, item_id: null, status: "running", created: 1, updated: 2, queued: 0, harness: "codex" })
function harness(local = { getItem: () => "1" }) {
  const state = { workspace: { id: "ws", item_prefix: "ST" }, workspaces: [], bots: [], allBots: [], projects: [], tags: [], settings: {}, devices: [], runtimes: [] }
  let onEvent, onOpen, cursorFn, opens = 0, closes = 0
  const timers = [], cues = [], notices = [], pageWs = [], looks = []
  const api = { shell: async () => structuredClone(state), summary: async () => ({ thinking: false, hold: null }), transcript: async () => page(), transcriptAfter: async () => page(), switchWs: async () => {} }
  const snapshot = { rev: 1, decision: [], item: [], session: [session("a"), session("b")] }, views = fakeSync(snapshot)
  const env = {
    require: (id) => {
      if (id === "@/thread/chat-progress") return chatProgress
      if (id === "./api" || id === "../api") return { api, setPageWorkspace: (ws) => { pageWs.push(ws) }, stream: (event, open, cursor) => { onEvent = event; onOpen = open; cursorFn = cursor; opens++; return () => { closes++ } }, Unauthenticated: class extends Error {} }
      if (id === "./data/live") return chat
      if (id === "./data/transcript") return transcript
      if (id === "./data/transcript-model" || id === "./transcript-model") return model
      if (id === "./data/sync" || id === "./sync") return views
      if (id === "./stream-cursor") return streamCursor
      if (id === "./live-utils" || id === "../live-utils") return live
      if (id === "./decision-notes") return notes
      if (id === "./i18n/decision") return decisionText
      if (id === "./decision-options") return decisionOptions
      if (id === "./feed-order") return feed
      if (id === "./i18n/format") return format
      if (id === "./i18n" || id === "../i18n") return { __esModule: true, ...i18n }
      if (id === "./composer-draft") return composerDraft
      if (id === "./runtime-status") return runtimeStatus
      if (id === "./item-state") return itemState
      if (id === "./lib/motion") return { applyLook: (x) => looks.push({ fontSize: x.fontSize, reduceMotion: x.reduceMotion }) }
      if (id === "./bus") return { cue: (x) => cues.push(x) }
      if (id === "./components/ui/toaster" || id === "../components/ui/toaster") return { toast: (x) => notices.push(x) }
      if (id === "@tanstack/react-query") return { isCancelledError }
      if (id === "react") return { useSyncExternalStore: (subscribe, read) => ({ subscribe, read }) }
      throw new Error(id)
    }, window: {}, document: { querySelector: () => null }, localStorage: local, location: {},
    setTimeout: (f, ms) => { const timer = { f, ms }; timers.push(timer); return timer }, clearTimeout: (timer) => { if (timer) timer.cancelled = true },
    requestAnimationFrame: (f) => { timers.push({ f, ms: 0 }); return 1 }, crypto: { randomUUID: () => "client-1" },
    AbortController, AbortSignal,
  }
  const load = (code) => { const exports = {}; runInNewContext(code, { ...env, exports }); return exports }
  const model = load(transcriptModelCode), transcript = load(transcriptCode), chat = load(liveCode), exports = load(code)
  const open = async (id) => { const release = transcript.observeTranscript(id); await flush(); return release }
  const tuples = (id) => transcript.getTranscript(id)?.turns.flatMap((turn) => turn.ev) ?? []
  return { patch: (kind, view) => views.applyPatch({ rev: ++snapshot.rev, ops: [{ type: kind, op: "upsert", id: view.id, view }] }), store: exports, chat, transcript, open, tuples, api, state, snapshot, cues, notices, pageWs, looks, event: (e) => onEvent(e), reconnect: (resumed) => onOpen(resumed), streamCursor: () => cursorFn?.(), opens: () => opens, closes: () => closes, cursorFn: () => cursorFn, tick: async (ms) => {
    const due = timers.splice(0); for (const t of due) { if (t.cancelled) continue; if (t.ms <= ms) t.f(); else timers.push(t) } await flush()
  } }
}

const manageSource = ts.createSourceFile("manage.tsx", readFileSync(new URL("../src/manage.tsx", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX)
function tagView(name, store, props = {}) {
  const fn = manageSource.statements.find((n) => ts.isFunctionDeclaration(n) && n.name?.text === name)
  assert.ok(fn, name)
  const source = `${name === "TagsPane" ? "export " : ""}${fn.getText(manageSource)}`
  const code = ts.transpileModule(source, { fileName: "tag-view.tsx", compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023, jsx: ts.JsxEmit.React, jsxFactory: "h", jsxFragmentFactory: "Fragment" } }).outputText
  const exports = {}, components = {}
  const visit = (node) => {
    if ((ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) && ts.isIdentifier(node.tagName)) components[node.tagName.text] = node.tagName.text
    ts.forEachChild(node, visit)
  }
  visit(fn)
  runInNewContext(code, {
    ...store, ...components, useTagUses: () => {}, exports, tagCat: "全部", Fragment: "Fragment",
    h: (type, props, ...children) => ({ type, props: { ...props, children } }),
    useT: () => (key) => key, useState: (initial) => [typeof initial === "function" ? initial() : initial, () => {}],
    catLabel: (_t, cat) => cat, closeSheet: () => {},
  })
  return exports[name](props)
}
const nodes = (node) => Array.isArray(node) ? node.flatMap(nodes) : node && typeof node === "object" ? [node, ...nodes(node.props?.children)] : []

test("category tags require both the stored category and one of the six reserved names", () => {
  const s = harness().store
  for (const name of ["bug", "feat", "doc", "refactor", "ops", "chore"]) {
    assert.equal(s.isKind({ name, cat: "类型" }), true, name)
    assert.equal(s.isKind({ name, cat: "领域" }), false, name)
  }
  for (const name of ["旧标签", "BUG", " bug", "bug ", "文档", ""]) assert.equal(s.isKind({ name, cat: "类型" }), false, name)
  assert.equal(s.isKind(), false)
})

test("kind lookup and sorting keep nonreserved legacy type tags among ordinary tags", () => {
  const s = harness().store
  s.st.tags = [
    { id: "legacy", name: "旧标签", cat: "类型" }, { id: "report", name: "报告", cat: "交付" },
    { id: "topic", name: "文档", cat: "主题" }, { id: "wrong", name: "doc", cat: "领域" },
    ...["chore", "ops", "refactor", "doc", "feat", "bug"].map((name) => ({ id: `kind-${name}`, name, cat: "类型" })),
  ]
  assert.deepEqual(Array.from(s.kindTags(), (t) => t.name), ["bug", "feat", "doc", "refactor", "ops", "chore"])
  assert.equal(s.kindOf({ tags: ["legacy", "wrong", "kind-doc"] }), "kind-doc")
  assert.equal(s.kindOf({ tags: ["legacy", "wrong", "missing"] }), undefined)
  const input = ["legacy", "topic", "report", "kind-doc", "missing"]
  assert.deepEqual(Array.from(s.sortTags(input)), ["kind-doc", "report", "legacy", "topic", "missing"])
  assert.deepEqual(input, ["legacy", "topic", "report", "kind-doc", "missing"])
})

test("switching kind preserves ordinary legacy tags and does not mistake one for the current kind", () => {
  const h = harness(), s = h.store, sent = []
  h.api.patchItem = async (id, patch) => { sent.push([id, Array.from(patch.tags)]) }
  s.st.tags = [{ id: "legacy", name: "旧标签", cat: "类型" }, { id: "doc", name: "doc", cat: "类型" }, { id: "bug", name: "bug", cat: "类型" }]
  h.patch("item", { id: "ST-1", state: { kind: "active" }, cycle: 1, revision: 0, delivery: [], tags: ["legacy", "doc"] })
  s.setKind("ST-1", "bug")
  assert.deepEqual(sent, [["ST-1", ["bug", "legacy"]]])
  h.patch("item", { id: "ST-1", state: { kind: "active" }, cycle: 1, revision: 0, delivery: [], tags: ["legacy", "bug"] })
  s.setKind("ST-1", "bug")
  assert.equal(sent.length, 1, "the real current kind is a no-op")
})

test("tag management renders legacy type tags with ordinary edit, merge and delete actions", () => {
  const s = harness().store
  s.st.tags = [
    { id: "legacy", name: "旧标签", cat: "类型", desc: "old", by: "me" },
    { id: "doc", name: "doc", cat: "类型", desc: "docs", by: "Steve" },
    { id: "topic", name: "主题标签", cat: "主题", desc: "topic", by: "me" },
  ]
  const tree = nodes(tagView("TagsPane", s)), legacy = tree.find((n) => n.props?.["data-tag-row"] === "旧标签"), builtin = tree.find((n) => n.props?.["data-tag-row"] === "doc")
  assert.ok(legacy, "the category section must not hide nonreserved tags")
  assert.ok(builtin)
  assert.ok(nodes(legacy).some((n) => n.props?.["data-del-tag"]))
  assert.ok(nodes(legacy).some((n) => n.props?.["data-merge-tag"]))
  assert.ok(!nodes(legacy).some((n) => n.type === "KindTag"))
  assert.ok(!nodes(builtin).some((n) => n.props?.["data-del-tag"] || n.props?.["data-merge-tag"]))
})

test("legacy tag editing preserves its current category without offering type for new ordinary tags", () => {
  const s = harness().store
  s.st.tags = [{ id: "legacy", name: "旧标签", cat: "类型", desc: "old" }, { id: "doc", name: "doc", cat: "类型", desc: "docs" }]
  const legacy = nodes(tagView("TagDialog", s, { id: "legacy" }))
  assert.equal(legacy.find((n) => n.props?.id === "tag-name").props.disabled, false)
  const selector = legacy.find((n) => n.props?.testId === "tag-cat")
  assert.equal(selector.props.value, "类型")
  assert.ok(selector.props.options.some((o) => o.v === "类型"), "an unchanged legacy category remains selectable")
  const fresh = nodes(tagView("TagDialog", s))
  assert.ok(fresh.find((n) => n.props?.testId === "tag-cat").props.options.every((o) => o.v !== "类型"))
  const builtin = nodes(tagView("TagDialog", s, { id: "doc" }))
  assert.equal(builtin.find((n) => n.props?.id === "tag-name").props.disabled, true)
  assert.ok(!builtin.some((n) => n.props?.testId === "tag-cat"))
})

test("stream patches preserve unrelated sessions, bots, items and thread references", async () => {
  const h = harness(), s = h.store
  h.snapshot.item = [{ id: "ST-1", state: { kind: "active" }, cycle: 1, revision: 0, delivery: [] }]
  s.start(); await flush(); await h.open("a"); await h.open("b")
  const before = { b: s.getSession("b"), bots: s.st.bots, item: s.getItem("ST-1"), version: s.st.ver }
  h.event({ type: "event", session_id: "a", seq: 1, kind: "text", data: { role: "assistant", text: "hello" } })
  await h.tick(80)
  assert.equal(h.tuples("a")[0][1], "hello")
  assert.equal(s.getSession("b"), before.b); assert.equal(s.st.bots, before.bots); assert.equal(s.getItem("ST-1"), before.item); assert.equal(s.st.ver, before.version)
})

test("item references never focus this request or the next one", async () => {
  const h = harness(), s = h.store, sent = []
  h.api.say = async (...args) => { sent.push(args) }
  for (const text of ["#ST-206 的结果作为背景，修复登录", "ST-206 只是上下文", "继续新的工作"]) {
    await s.chatSend(text)
    assert.equal(sent.at(-1)[3].item, undefined)
    assert.equal(s.focusItem(), null)
    assert.equal(s.st.focus, null)
  }
})

test("only an open explicit focus or quote directs a message to an item", async () => {
  const h = harness(), s = h.store, sent = []
  h.api.say = async (...args) => { sent.push(args) }
  h.patch("item", { id: "ST-1", state: { kind: "active" }, cycle: 1, revision: 0, delivery: [] }); h.patch("item", { id: "ST-2", state: { kind: "closed", outcome: "finished", by: "owner" }, cycle: 1, revision: 1, delivery: [] })
  s.st.focus = { item: "ST-1", until: Date.now() + 60_000 }
  await s.chatSend("记得覆盖跨日统计")
  assert.equal(sent.at(-1)[3].item, "ST-1")
  assert.equal(s.focusItem(), "ST-1")
  h.patch("item", { id: "ST-1", state: { kind: "closed", outcome: "finished", by: "owner" }, cycle: 1, revision: 1, delivery: [] })
  assert.equal(s.focusItem(), null)
  await s.chatSend("修复另一个页面")
  assert.equal(sent.at(-1)[3].item, undefined)
  assert.equal(s.st.focus, null)

  h.patch("message", { ...chatMessage("old", "steve"), item_id: "ST-2" })
  s.st.quote = "old"
  await s.chatSend("根据旧交付，再修登录页")
  assert.equal(sent.at(-1)[3].reply_to, "old", "the quote remains available as context")
  assert.equal(s.st.focus, null, "quoting a finished item cannot capture later messages")
})

test("waiting patches update Node reasons without refreshing state or rebuilding the transcript", async () => {
  const h = harness(), s = h.store, row = h.snapshot.session[0]
  const counts = counted(h)
  row.waiting = { reason: "capacity" }
  s.start(); await flush(); await h.open("a")
  const events = h.transcript.getTranscript("a")
  assert.equal(s.getSession("a").room.reason, "capacity")
  h.event({ type: "patch", rev: 2, ops: [{ type: "session", op: "upsert", id: "a",
    view: { ...row, waiting: { reason: "node", detail: "journal storage full" } } }] })
  assert.equal(s.getSession("a").room.detail, "journal storage full")
  h.event({ type: "patch", rev: 3, ops: [{ type: "session", op: "upsert", id: "a",
    view: { ...row, status: "ended" } }] })
  await h.tick(1000)
  assert.equal(s.getSession("a").room, null)
  assert.equal(counts.states, 1)
  // The transcript remains loaded when the metadata changes.
  assert.equal(h.transcript.getTranscript("a"), events)
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
const deliverMessage = (h, view) => h.event({ type: "patch", rev: ++h.snapshot.rev, ops: [{ type: "message", op: "upsert", id: view.id, view }] })
const draft = (h, mid, text) => h.event({ type: "steve.draft", ws: "ws", mid, thought: "", text })

test("chat drafts paint successive chunks before any stored reply arrives", async () => {
  const h = harness(), s = h.store, c = counted(h)
  h.snapshot.message = [chatMessage("question", "me")]
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true, phase: "answer" })
  const version = s.st.ver
  draft(h, "question", "第一段"); await h.tick(0)
  assert.equal(h.chat.chatLive().draft.text, "第一段"); assert.equal(s.st.ver, version)
  draft(h, "question", "第一段，第二段"); await h.tick(0)
  assert.equal(h.chat.chatLive().draft.text, "第一段，第二段")
  assert.equal(c.states, 1)
})

test("draft text publishes at a low cadence, but a stored reply takes precedence immediately", async () => {
  const h = harness()
  h.store.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  const { subscribe, read } = h.chat.useChatLive((s) => s.draft?.text)
  const painted = []
  const off = subscribe(() => painted.push(read()))
  draft(h, "question", "a")
  draft(h, "question", "ab")
  assert.equal(h.chat.chatLive().draft.text, "ab", "ingests updates for the matching persisted reply")
  assert.equal(read(), undefined)
  await h.tick(0)
  assert.deepEqual(painted, ["ab"])
  draft(h, "question", "abc")
  draft(h, "question", "abcd")
  await h.tick(0)
  assert.deepEqual(painted, ["ab"])
  await h.tick(h.chat.REPLY_BATCH_MS)
  assert.deepEqual(painted, ["ab", "abcd"])
  draft(h, "question", "abcde")
  deliverMessage(h, chatMessage("answer", "steve", "question"))
  assert.deepEqual(painted, ["ab", "abcd", undefined])
  await h.tick(h.chat.REPLY_BATCH_MS)
  assert.deepEqual(painted, ["ab", "abcd", undefined], "no stale batch resurrects the draft")
  off()
})

test("an unpainted draft cannot publish into another workspace", async () => {
  const h = harness()
  h.store.start(); await flush()
  draft(h, "question", "old workspace")
  await h.tick(0)
  draft(h, "question", "unpainted old workspace body")
  await h.store.switchWorkspace("other")
  await h.tick(h.chat.REPLY_BATCH_MS)
  assert.equal(h.chat.useChatLive((s) => s.draft).read(), null)
})

test("unrelated Steve messages keep the live draft; only its own reply replaces it", async () => {
  const h = harness(), s = h.store
  h.snapshot.message = [chatMessage("question", "me")]
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "question", "仍在生成")
  deliverMessage(h, chatMessage("item-progress", "steve", "other-question"))
  assert.equal(h.chat.chatLive().draft.text, "仍在生成"); assert.equal(h.chat.chatLive().drafted, "")
  deliverMessage(h, chatMessage("answer", "steve", "question"))
  assert.equal(h.chat.chatLive().draft, null); assert.equal(h.chat.chatLive().drafted, "answer")
  assert.equal(s.chatMessage("answer")?.id, "answer")
})

test("rerunning a question does not replace its new draft with the previous reply", async () => {
  const h = harness(), s = h.store
  h.snapshot.message = [chatMessage("question", "me"), chatMessage("previous-answer", "steve", "question")]
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "question", "重新生成")
  await s.refresh()
  assert.equal(h.chat.chatLive().draft.text, "重新生成"); assert.equal(h.chat.chatLive().drafted, "")
  deliverMessage(h, chatMessage("new-answer", "steve", "question"))
  h.event({ type: "steve.thinking", on: false }); await s.refresh()
  assert.equal(h.chat.chatLive().draft, null); assert.equal(h.chat.chatLive().drafted, "new-answer")
})

test("a state request begun while idle cannot clear a turn that started during the request", async () => {
  const h = harness(), s = h.store, response = defer()
  s.start(); await flush()
  const old = await h.api.shell()
  h.api.shell = () => response.promise
  const loading = s.refresh()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "new-question", "新回复")
  response.resolve(old); await loading
  assert.equal(h.chat.chatLive().draft.text, "新回复"); assert.equal(h.chat.chatLive().thinking, true)
})

test("a stopped turn's refresh cannot clear the next turn's draft", async () => {
  const h = harness(), s = h.store, response = defer()
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "first-question", "第一条")
  h.event({ type: "steve.thinking", on: false })
  const old = await h.api.shell()
  h.api.shell = () => response.promise
  const loading = s.refresh()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "next-question", "第二条")
  response.resolve(old); await loading
  assert.equal(h.chat.chatLive().draft.text, "第二条"); assert.equal(h.chat.chatLive().thinking, true)
})

test("a settled turn without a stored reply clears its draft locally", async () => {
  const h = harness(), s = h.store
  s.start(); await flush()
  h.event({ type: "steve.thinking", on: true })
  draft(h, "question", "尚未保存")
  h.event({ type: "steve.thinking", on: false }); await h.tick(1500)
  assert.equal(h.chat.chatLive().draft, null)
  assert.equal(h.chat.chatLive().thinking, false)
})

test("one tail page opens, merges pending SSE once and prepends aligned turns", async () => {
  const h = harness(), tail = defer(), cursors = []
  h.api.transcript = async (_sid, before) => {
    cursors.push(before)
    return before == null ? tail.promise : page([{ seq: 2, ts: 2, kind: "user", data: { text: "earlier" } }])
  }
  h.store.start(); await flush()
  const opening = h.open("a")
  h.event({ type: "event", session_id: "a", ...chunk(7, "B") })
  tail.resolve(page([{ seq: 3, ts: 3, kind: "user", data: { text: "latest" } }, chunk(5, "A"), chunk(7, "B")], 3))
  await opening
  assert.deepEqual(cursors, [undefined])
  assert.equal(h.tuples("a").at(-1)[1], "AB")
  h.event({ type: "event", session_id: "a", ...chunk(13, "C") })
  await h.tick(16)
  const tailTurn = h.transcript.getTranscript("a").turns[0]
  await h.transcript.loadOlderTranscript("a")
  assert.deepEqual(cursors, [undefined, 3])
  assert.equal(h.transcript.getTranscript("a").turns.at(-1), tailTurn)
  assert.equal(h.tuples("a").at(-1)[1], "ABC")
})

test("a workspace metadata event refreshes the shell without recovering transcripts", async () => {
  const h = harness(), s = h.store
  let details = 0, events = 0, resyncs = 0
  h.api.item = async () => { details++; return { dependents: [] } }
  s.start(); await flush(); await s.loadItem("loaded"); await h.open("a")
  h.api.transcriptAfter = async () => { events++; return page() }
  s.hubSubs.add((e) => { if (e.type === "resync") resyncs++ })
  h.reconnect(); await flush()
  h.event({ type: "workspace", id: "ws" }); await flush()
  assert.equal(resyncs, 1); assert.equal(details, 1); assert.equal(events, 1)
})

/* C1: count what a reconnect refetches */
function counted(h) {
  const c = { states: 0, items: [], after: [], resyncs: 0 }
  const state = h.api.shell; h.api.shell = () => { c.states++; return state() }
  h.api.item = async (id) => { c.items.push(id); return { dependents: [] } }
  h.store.hubSubs.add((e) => { if (e.type === "resync") c.resyncs++ })
  return c
}

test("a resumed reconnect refetches only what the replay touched, and the gap's transcript once", async () => {
  const h = harness(), s = h.store, c = counted(h)
  h.snapshot.cursor = "E:5"
  s.start(); await flush()
  assert.equal(h.opens(), 1); assert.equal(h.streamCursor(), "E:5"); assert.equal(c.states, 1) // the stream opens after the state it continues
  const log = [1]
  h.api.transcript = async () => page(log.map((seq) => chunk(seq)))
  h.api.transcriptAfter = async (_sid, after) => { c.after.push(after); return page(log.filter((seq) => seq > after).map((seq) => chunk(seq))) }
  await s.loadItem("A"); await s.loadItem("B"); await h.open("a")
  c.items.length = 0; c.after.length = 0
  log.push(2, 3) // written while the page was away
  h.reconnect(true); await flush()
  assert.equal(c.resyncs, 0); assert.equal(c.states, 1)
  h.event({ type: "patch", rev: 2, ops: [{ type: "item", op: "upsert", id: "A", view: { id: "A", star: true } }] }) // replayed
  h.event({ type: "stream.gap" })
  h.event({ type: "event", session_id: "a", seq: 3, kind: "text", data: { role: "assistant", text: "C" } }) // live, before the catch-up answers
  await h.tick(1000); await h.tick(1000)
  assert.deepEqual(c.items, []); assert.equal(c.states, 1); assert.equal(c.resyncs, 0)
  assert.equal(c.after[0], 1) // from before the gap, not from the live frame (then the next page, and the frame seen during the load)
  assert.ok(c.after.every((x) => x >= 1), String(c.after))
  await h.tick(80); assert.equal(h.tuples("a").length, 1); assert.equal(h.tuples("a")[0][1], "ABC") // every missed chunk once
})

test("a stream reset resyncs everything once", async () => {
  const h = harness(), s = h.store, c = counted(h)
  h.snapshot.cursor = "E:5"
  s.start(); await flush(); await s.loadItem("A"); await s.loadItem("B")
  c.items.length = 0
  h.reconnect(true); h.event({ type: "stream.reset" }); await flush()
  assert.equal(c.resyncs, 1); assert.equal(c.states, 2); assert.deepEqual(c.items, [])
})

test("without a cursor every open resyncs, and the stream is reopened from the state that resync loaded", async () => {
  const h = harness(), s = h.store, c = counted(h)
  s.start(); await flush()
  assert.equal(h.streamCursor(), null)
  h.reconnect(false); await flush() // an old Hub: no watermark, the stream stays as it is
  assert.equal(c.resyncs, 1); assert.equal(h.opens(), 1); assert.equal(h.streamCursor(), null)
  h.snapshot.cursor = "E:9"
  h.reconnect(false); await flush()
  assert.equal(c.resyncs, 2); assert.equal(h.opens(), 2); assert.equal(h.closes(), 1); assert.equal(h.streamCursor(), "E:9")
})

test("a research tool call is on the live line while it runs, and a step once it ends", async () => {
  const h = harness(), s = h.store
  h.snapshot.message = [chatMessage("question", "me")]
  s.start(); await flush()
  const tool = (run, extra) => h.event({ type: "steve.tool", ws: "ws", mid: "question", run, name: "Bash", brief: "du -sh ~", ok: true, ...extra })
  const live = () => JSON.parse(JSON.stringify({ run: h.chat.chatLive().live.run, now: h.chat.chatLive().live.now ?? null, tools: h.chat.chatLive().live.tools }))
  tool("tr_1", { kind: "exec", running: true }); await h.tick(0)
  assert.deepEqual(live(), { run: "tr_1", now: { name: "Bash", brief: "du -sh ~", kind: "exec" }, tools: [] })
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
  h.snapshot.cursor = "E:9"
  h.reconnect(false) // the legacy open: its resync loads E:9
  h.event({ type: "steve.tool", ws: "ws", mid: "m1", run: "r1", name: "search", brief: "q", ok: true }) // live, no frame id
  await flush()
  assert.equal(h.chat.chatLive().live.tools.length, 1)
  // before: this stream's next reconnect sent ?after=E:9 and the Hub replayed the tool row (and everything else) again
  assert.equal(legacyCursor(), null); assert.equal(h.closes(), 1)
  assert.equal(h.opens(), 2); assert.equal(h.streamCursor(), "E:9") // reopened from the watermark: its frames carry ids
  h.reconnect(true); await flush() // a resumed open of the new stream resyncs nothing and reopens nothing
  assert.equal(h.opens(), 2); assert.equal(h.chat.chatLive().live.tools.length, 1)
})

test("a stopped store does not reopen the stream after a legacy resync", async () => {
  const h = harness(), s = h.store
  const stop = s.start(); await flush()
  h.snapshot.cursor = "E:9"
  h.reconnect(false); stop(); await flush()
  assert.equal(h.opens(), 1)
})

test("a workspace switch opens a new stream from the new workspace's state", async () => {
  const h = harness(), s = h.store
  h.snapshot.cursor = "E:5"
  s.start(); await flush()
  h.snapshot.cursor = "E:12"
  await s.switchWorkspace("other")
  assert.equal(h.opens(), 2); assert.equal(h.streamCursor(), "E:12")
})

test("old workspace event pages cannot populate the new workspace", async () => {
  const h = harness(), s = h.store, page = defer()
  s.start(); await flush(); h.api.transcript = async () => page.promise
  const loading = h.open("a"); await s.switchWorkspace("other")
  page.resolve({ events: [chunk(1)], next: null }); await loading; await flush()
  assert.equal(h.transcript.getTranscript("a"), undefined)
})

test("session and decision actions guard duplicates and report no early success", async () => {
  const h = harness(), s = h.store, send = defer(), decision = defer(); let sends = 0, decisions = 0
  h.snapshot.decision = [{ id: "d", ws: "ws", kind: "accept", session_id: "a", subject_type: "item", question: { kind: "raw", text: "ok?" }, options: [], status: "open", created: 1 }]
  s.start(); await flush()
  h.api.sessionSay = (_id, _text, mode) => { assert.equal(mode, "排队"); sends++; return send.promise }
  const sending = s.sessSend("a", "hello", "queue"); assert.equal(s.st.pendingActions.has("session:a"), true)
  assert.equal(await s.sessSend("a", "hello", "queue"), false); assert.equal(sends, 1); assert.equal(h.notices.length, 0)
  send.resolve({}); await sending; assert.equal(s.st.pendingActions.size, 0)
  h.api.decide = () => { decisions++; return decision.promise }
  const resolving = s.decide("", "pass", undefined, "d"); await s.decide("", "pass", undefined, "d")
  assert.equal(decisions, 1); assert.equal(h.cues.includes("success"), false); assert.equal(s.st.pendingActions.has("decision:d"), true)
  decision.reject(new Error("denied")); await resolving
  assert.equal(s.st.pendingActions.size, 0); assert.equal(h.cues.includes("success"), false)
})

test("failed gap catch-up retries from the same cursor before applying live patches", async () => {
  const h = harness(), s = h.store, cursors = []
  h.api.transcript = async () => page([chunk(1)])
  s.start(); await flush(); await h.open("a")
  h.api.transcriptAfter = async (_sid, after) => {
    cursors.push(after)
    if (cursors.length === 1) throw new Error("offline")
    return page([chunk(2)])
  }
  h.event({ type: "stream.gap" })
  h.event({ type: "event", session_id: "a", ...chunk(3) })
  await flush(); await h.tick(2000)
  assert.deepEqual(cursors, [1, 1]); assert.equal(h.tuples("a")[0][1], "ABC")
})

test("resync caps transcript catch-up at four concurrent requests", async () => {
  const h = harness(), s = h.store
  h.snapshot.session = Array.from({ length: 9 }, (_, n) => session(String(n)))
  s.start(); await flush(); for (const sess of h.snapshot.session) await h.open(sess.id)
  let active = 0, peak = 0, calls = 0
  h.api.transcriptAfter = async () => { calls++; peak = Math.max(peak, ++active); await new Promise((r) => setImmediate(r)); active--; return page() }
  h.reconnect()
  for (let i = 0; i < 10; i++) await new Promise((r) => setImmediate(r))
  assert.equal(calls, 9); assert.equal(peak, 4)
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
    h.snapshot.item = [{ id: "ST-1", title: "x", state: { kind: "active" }, cycle: 1, revision: 0, delivery: [], accept_mode: "auto", deps: [], source: [], tags: [], plan: [], sessionIds: [], timeline: [], created: 1, updated: 2 }]
    h.api.patchItem = async (id, p) => { sent.push([id, p]); return {} }
    s.start(); await flush()
    const i = s.getItem("ST-1")
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
  h.state.owner = true; h.snapshot.session = []; h.snapshot.cursor = "E:1"
  h.api.onboardStart = async () => { calls.push("start"); h.state.settings = { onboard: "pending", onboardFrom: "hello" }; deliverMessage(h, { id: "hello", author: "steve", text: "你好，我是 Steve。", created: 1, item_id: null, meta: { onboard: true } }); return { message: "hello" } }
  h.api.onboardSkip = async () => { calls.push("skip"); h.state.settings = { onboard: "done", onboardFrom: "hello" } }
  s.start(); await flush()
  assert.equal(s.st.onboard, 0); assert.equal(s.st.owner, true); assert.equal(s.meeting(), false)
  await s.meet()
  assert.deepEqual(calls, ["start"]); assert.equal(s.st.onboard, -1); assert.equal(local.kept.get("steve-onboarded"), "1")
  assert.equal(s.meeting(), true); assert.equal(s.chatMessage("hello").meet, true)
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
  h.state.owner = true; h.snapshot.session = []; h.snapshot.cursor = "E:1"
  h.api.onboardSkip = async () => { calls.push("skip"); h.state.settings = { onboard: "done" } }
  s.start(); await flush()
  assert.equal(s.st.onboard, 0)
  s.skipOnboarding(); await flush()
  assert.deepEqual(calls, ["skip"]); assert.equal(s.st.onboard, -1); assert.equal(s.st.settings.onboard, "done")
  /* a member has no first meeting: the tour and its skip stay in the browser */
  const theirs = browser(), m = harness(theirs)
  m.snapshot.cursor = "E:1"
  m.state.sessions = []; m.api.onboardSkip = async () => { calls.push("member") }
  m.store.start(); await flush()
  assert.equal(m.store.st.onboard, 0); assert.equal(m.store.st.owner, false)
  m.store.skipOnboarding(); await flush()
  assert.deepEqual(calls, ["skip"]); assert.equal(m.store.st.onboard, -1); assert.equal(theirs.kept.get("steve-onboarded"), "1")
})

test("defaultHarness, permModes, defaultPermMode and makeBot adapt across heterogeneous runtimes", async () => {
  const h = harness(), s = h.store, created = []
  h.api.newBot = async (p) => { created.push(p); return { id: "bot-2" } }
  s.setRuntimes([
    { id: "claude-code", name: "Claude Code", ok: false, modes: ["plan", "ask", "accept_edits", "auto", "full_access"] },
    { id: "cursor", name: "Cursor", ok: true, modes: ["plan", "ask"] },
    { id: "opencode", name: "OpenCode", ok: true, modes: ["plan", "auto"] },
    { id: "custom-empty", name: "Custom Empty", ok: true, modes: [] },
    { id: "custom-exotic", name: "Custom Exotic", ok: true, modes: ["weird_mode"] },
  ])
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
  assert.deepEqual([...s.permModes("opencode")], ["plan"])
  assert.equal(s.permModes("custom-empty").length, 1)
  assert.deepEqual([...s.permModes("custom-exotic")], ["default"])

  // defaultPermMode picks a compatible mode in priority order (default -> plan, with full access only for a native-only runtime)
  assert.equal(s.defaultPermMode("cursor"), "default")
  assert.equal(s.defaultPermMode("opencode"), "plan")
  assert.equal(s.defaultPermMode("custom-exotic"), "default")

  // makeBot sends a compatible perm_mode for the selected harness
  const id1 = await s.makeBot("Worker A", "coder", "opencode")
  assert.equal(id1, "bot-2")
  assert.equal(created[0].harness, "opencode")
  assert.equal(created[0].perm_mode, "plan")

  const id2 = await s.makeBot("Worker B", "coder")
  assert.equal(id2, "bot-2")
  assert.equal(created[1].harness, "cursor")
  assert.equal(created[1].perm_mode, "default")
})

test("decision patches change the cards and their item without reading /api/shell again", async () => {
  const h = harness(), s = h.store
  let reads = 0; const read = h.api.shell; h.api.shell = async () => { reads++; return read() }
  h.snapshot.item = [{ id: "ST-1", no: 1, title: "x", state: { kind: "active" }, cycle: 1, revision: 0, delivery: [], plan: [], sessionIds: [], tags: [], deps: [], source: ["你", "web"], created: 1, updated: 1, goal: "g" }]
  h.snapshot.decision = [{ id: "d1", ws: "ws", kind: "ask", item_id: "ST-1", session_id: null, subject_type: "item", subject_id: "ST-1", question: { kind: "raw", text: "ok?" }, draft: "", options: [], status: "open", created: 1, tier: "decide" }]
  s.start(); await flush()
  assert.deepEqual(Array.from(s.pendingCards(), (c) => c.w.decision), ["d1"])
  assert.equal(s.decideCards().length, 1)
  assert.equal(s.it("ST-1").waits.length, 1)
  const before = reads, ver = s.st.ver, item = s.it("ST-1")
  h.event({ type: "patch", ws: "ws", rev: 5, ops: [{ type: "decision", op: "upsert", id: "d1", view: { ...h.snapshot.decision[0], status: "resolved" } }, { type: "decision", op: "upsert", id: "d2", view: { ...h.snapshot.decision[0], id: "d2", item_id: null, question: { kind: "raw", text: "new?" }, subject_type: "chan_message", kind: "approve", tier: "draft", space_id: "sp" } }] })
  await h.tick(1000)
  assert.equal(reads, before, "no /api/shell read for a decision")
  assert.ok(s.st.ver > ver)
  assert.equal(s.it("ST-1"), item, "a patch does not rebuild the item")
  assert.equal(item.waits.length, 0)
  assert.deepEqual(Array.from(s.pendingCards(), (c) => c.w.q), ["new?"], "an open decision without an item is a card of its own")
  assert.equal(s.decideCards().length, 0, "a draft is not counted in the badge")
  // an older frame changes nothing
  h.event({ type: "patch", ws: "ws", rev: 4, ops: [{ type: "decision", op: "upsert", id: "d1", view: h.snapshot.decision[0] }] })
  assert.equal(s.it("ST-1").waits.length, 0)
})
