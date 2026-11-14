import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "typescript"

const calls = [], exports = {}
let hub = "https://first.example"
const code = ts.transpileModule(readFileSync(new URL("../src/api/feed.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
runInNewContext(code, { exports, URL, AbortSignal, require: (id) => {
  if (id === "@/hub") return { currentHub: () => hub }
  assert.equal(id, "@/api")
  return { qs: (params) => "?" + new URLSearchParams(Object.entries(params).filter(([, value]) => value !== undefined)), modReq: async (...args) => { calls.push(args); return { updated: 1 } } }
} })
const { feedApi, feedHref, appendFeed } = exports
const plain = (value) => JSON.parse(JSON.stringify(value))
test("Feed binds actual requests to its captured Hub/workspace; reads are explicit and preserve opaque tokens/filters", async () => {
  calls.length = 0
  const client = feedApi.forWorkspace("workspace-a")
  hub = "https://second.example"
  const filters = { kind: "completion", source: "notification", unread: true }
  await client.list(filters, "opaque/+=", 50)
  await client.detail("id/space value")
  assert.equal(calls.length, 2, "reading never implicitly posts read state")
  await client.read({ ids: ["id/space value"] })
  await client.read({ snapshot: "signed-snapshot", ...filters })
  assert.equal(calls[0][1], "/me/feed?kind=completion&source=notification&unread=true&cursor=opaque%2F%2B%3D&limit=50")
  assert.equal(calls[1][1], "/me/feed/id%2Fspace%20value")
  assert.deepEqual(plain(calls.slice(2).map((call) => call.slice(0, 3))), [["POST", "/me/feed/read", { ids: ["id/space value"] }], ["POST", "/me/feed/read", { snapshot: "signed-snapshot", ...filters }]])
  for (const call of calls) {
    assert.deepEqual(plain(call[4]), { workspace: "workspace-a", hub: "https://first.example" })
    assert.ok(call[3] instanceof AbortSignal)
  }
})
test("Feed navigation URLs reject executable, protocol-relative, whitespace and backslash tricks", () => {
  for (const value of ["javascript:alert(1)", "data:text/html,bad", "//evil.example", "/\\evil.example", " https://example.com", "https://example.com/\nattack", "file:///secret", "mailto:person@example.com"]) assert.equal(feedHref(value), null, value)
  for (const value of ["#/items/ST-1", "/api/resource", "https://example.com/path", "http://example.com/path"]) assert.equal(feedHref(value), value)
})
test("keyset pages retain reading order without duplicate boundary rows", () => {
  const entries = appendFeed([{ id: "a" }, { id: "b" }], [{ id: "b" }, { id: "c" }, { id: "c" }])
  assert.deepEqual(plain(entries).map((entry) => entry.id), ["a", "b", "c"])
})
