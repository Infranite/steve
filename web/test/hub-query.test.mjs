import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import { test } from "node:test"
import ts from "typescript"

const source = ts.transpileModule(
  readFileSync(new URL("../src/data/use-hub-query.ts", import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } },
).outputText

class HubError extends Error {
  status
  constructor(status, msg) { super(msg); this.status = status }
}
class Unauthenticated extends Error {
  login
  constructor(login) { super("unauthenticated"); this.login = login }
}

function harness(queryResult, initialScope = 1) {
  let scope = initialScope, emitted = 0, lastQueryOpts = null, lastEvents = null, lastSet = null
  const st = { login: null }
  const qc = { setQueryData: (key, v) => { lastSet = { key, v } } }
  const exports = {}
  runInNewContext(source, {
    exports,
    require: (id) => {
      if (id === "react") return {
        useCallback: (fn) => fn,
        useRef: (initial) => ({ current: initial }),
      }
      if (id === "@tanstack/react-query") return {
        useQuery: (opts) => { lastQueryOpts = opts; return queryResult },
      }
      if (id === "@/api") return { HubError, Unauthenticated }
      if (id === "@/data/sync") return { qc, scopeKey: () => scope }
      if (id === "@/store") return {
        st,
        emit: () => { emitted++ },
        useHubEvents: (events, fn, ms) => { lastEvents = { events, fn, ms } },
      }
      throw new Error(`unexpected require: ${id}`)
    },
  })
  return {
    exports,
    st,
    get emitted() { return emitted },
    get lastQueryOpts() { return lastQueryOpts },
    get lastEvents() { return lastEvents },
    get lastSet() { return lastSet },
    setScope(n) { scope = n },
  }
}

test("useHubQuery scopes keys by workspace, surfaces HubError status, and wires reload/set", async () => {
  let refetches = 0
  const h = harness({
    data: { items: [1] },
    error: new HubError(404, "missing"),
    isPending: false,
    dataUpdatedAt: 12345,
    refetch: async () => { refetches++ },
  }, 7)
  const q = h.exports.useHubQuery(["mail", "thread", "t1"], async () => ({ items: [1] }), { events: ["mail"], debounceMs: 200 })
  assert.deepEqual([...h.lastQueryOpts.queryKey], ["hub", 7, "mail", "thread", "t1"])
  assert.equal(h.lastQueryOpts.staleTime, 0)
  assert.equal(q.status, 404)
  assert.equal(q.updatedAt, 12345)
  assert.deepEqual([...h.lastEvents.events], ["mail"])
  assert.equal(h.lastEvents.ms, 200)
  await q.reload()
  assert.equal(refetches, 1)
  h.lastEvents.fn()
  assert.equal(refetches, 2)
  q.set({ items: [2] })
  assert.deepEqual(JSON.parse(JSON.stringify(h.lastSet)), { key: ["hub", 7, "mail", "thread", "t1"], v: { items: [2] } })
})

test("useHubQuery routes Unauthenticated to sign-in without exposing an error state", async () => {
  const unauth = new Unauthenticated("/auth/dev")
  const h = harness({
    data: undefined,
    error: unauth,
    isPending: false,
    dataUpdatedAt: 0,
    refetch: async () => {},
  }, 2)
  const q = h.exports.useHubQuery(["stats", "summary"], async () => { throw unauth }, { events: ["stats"], live: false })
  assert.equal(q.error, null)
  assert.equal(q.loading, true)
  assert.deepEqual([...h.lastEvents.events], [])
  await assert.rejects(() => h.lastQueryOpts.queryFn({ signal: new AbortController().signal }), (e) => e === unauth)
  assert.equal(h.st.login, "/auth/dev")
  assert.equal(h.emitted, 1)
})
