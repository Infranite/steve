import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "typescript"

const exports = {}
const code = ts.transpileModule(readFileSync(new URL("../src/thread/attention-read.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
runInNewContext(code, { exports, window: { innerHeight: 900, innerWidth: 1200 } })
const { visibleReply, replySnapshot, visibleReplyIDs } = exports
const entry = (id, order) => ({ id, event_id: id, created: 1, order, target: { route: "", entity_id: id }, notify: true })
const page = (entries, cursor = null, revision = "r1", user = "u1") => ({ v: 1, epoch: "install", user_id: user, workspace_id: "ws1", history_policy: "since_attention_v1", categories: [{ kind: "replies", available: true, unchanged: false, revision, count: 4, unit: "messages", snapshot: "opaque-snapshot", entries, next_cursor: cursor }] })

test("read advances to the greatest server order actually visible, not the latest unread", () => {
  const entries = [entry("latest", 8), entry("visible", 4), entry("draft", undefined), entry("above", 1)]
  assert.equal(visibleReply(entries, new Set(["visible", "draft"])).id, "visible")
  assert.equal(visibleReply(entries, new Set(["draft", "unknown"])), undefined)
})
test("reply reader completes all pages before using a snapshot and never posts read itself", async () => {
  const calls = [], pages = [page([entry("a", 1)], "next"), page([entry("a", 1), entry("b", 2)])]
  const result = await replySnapshot({ replies: async (cursor) => { calls.push(cursor); return pages.shift() } }, "ws1", new AbortController().signal)
  assert.deepEqual(calls, [undefined, "next"])
  assert.deepEqual(Array.from(result.entries, e => e.id), ["a", "b"])
  assert.equal(result.snapshot, "opaque-snapshot")
})
test("changed user/revision, foreign workspace, unsupported and repeated cursor cannot advance reads", async () => {
  for (const pages of [
    [page([], "next"), page([], null, "r2")],
    [page([], "next"), page([], null, "r1", "u2")],
    [{ ...page([]), workspace_id: "foreign" }],
    [{ ...page([]), v: 2 }],
    [page([], "loop"), page([], "loop")],
  ]) await assert.rejects(replySnapshot({ replies: async () => pages.shift() }, "ws1", new AbortController().signal))
})
test("cancelled readers never issue another page request", async () => {
  const controller = new AbortController(); let calls = 0
  await assert.rejects(replySnapshot({ replies: async () => { calls++; controller.abort(); return page([], "next") } }, "ws1", controller.signal))
  assert.equal(calls, 1)
})
test("viewport measurement includes clipped long messages, excludes rows outside the viewport", () => {
  const rect = (top, bottom) => ({ top, bottom, left: 0, right: 800, width: 800, height: bottom - top })
  const rows = [["above", -100, 90], ["long", -100, 400], ["below", 800, 1000]].map(([id, top, bottom]) => ({ dataset: { msg: id }, getBoundingClientRect: () => rect(top, bottom) }))
  assert.deepEqual(Array.from(visibleReplyIDs({ getBoundingClientRect: () => rect(100, 700), querySelectorAll: () => rows })), ["long"])
})

test("attention API binds read and snapshot requests to their captured Hub and workspace", async () => {
  const ex = {}, calls = []; let hub = "https://first.example"
  const source = ts.transpileModule(readFileSync(new URL("../src/api/attention.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
  runInNewContext(source, { exports: ex, AbortSignal, require: id => id === "@/hub" ? { currentHub: () => hub } : { qs: params => "?" + new URLSearchParams(Object.entries(params).filter(([, value]) => value !== undefined)), modReq: async (...args) => { calls.push(args); return {} } } })
  const client = ex.attentionApi.forWorkspace("ws1"); hub = "https://second.example"
  await client.replies("opaque/+", new AbortController().signal)
  await client.read("signed-token", "visible-id", new AbortController().signal)
  assert.equal(calls[0][1], "/me/attention?kind=replies&limit=100&cursor=opaque%2F%2B")
  assert.deepEqual(JSON.parse(JSON.stringify(calls[1].slice(0, 3))), ["POST", "/me/attention/read", { snapshot: "signed-token", through_id: "visible-id" }])
  for (const call of calls) assert.deepEqual(JSON.parse(JSON.stringify(call[4])), { hub: "https://first.example", workspace: "ws1" })
})
