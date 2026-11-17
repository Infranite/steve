import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "typescript"

const compile = path => ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
const tick = () => new Promise(resolve => setImmediate(resolve))
function harness({ focused = true, failure, deferred } = {}) {
  const exports = {}, calls = [], timers = [], state = { ready: true, login: null, workspace: { id: "ws1" } }
  const doc = { visibilityState: "visible", hasFocus: () => focused, addEventListener() {}, removeEventListener() {} }
  let cleanup, interval
  const root = { addEventListener() {}, removeEventListener() {} }
  const snapshot = { snapshot: "signed", entries: [{ id: "on-screen", order: 1, target: { entity_id: "on-screen" } }, { id: "off-screen", order: 2, target: { entity_id: "off-screen" } }] }
  runInNewContext(compile("../src/thread/use-reply-reading.ts"), {
    exports, document: doc, AbortController, Date, Event,
    window: { addEventListener() {}, removeEventListener() {}, dispatchEvent() {} },
    MutationObserver: class { observe() {} disconnect() {} },
    setTimeout: f => { timers.push(f); return timers.length }, clearTimeout() {},
    setInterval: f => { interval = f; return 1 }, clearInterval() {},
    require: id => ({
      "react": { useEffect: f => { cleanup = f() } },
      "@/api": { isNotWired: e => e?.unsupported === true },
      "@/hub": { currentHub: () => "https://hub.example" },
      "@/store": { st: state },
      "@/api/attention": { attentionApi: { forWorkspace: () => ({ read: async (...args) => { calls.push(["read", ...args]) } }) } },
      "@/thread/attention-read": {
        replySnapshot: async () => { calls.push(["snapshot"]); if (failure) throw failure; if (deferred) await deferred; return snapshot },
        visibleReplyIDs: () => new Set(["on-screen"]),
        visibleReply: entries => entries.find(e => e.id === "on-screen"),
      },
    })[id],
  })
  exports.useReplyReading({ current: root })
  return { calls, state, doc, run: async () => { timers.shift()?.(); await tick() }, refresh: async () => { await interval(); await tick() }, cleanup: () => cleanup() }
}

test("background/hidden conversations do not fetch or mark final replies", async () => {
  const h = harness({ focused: false }); await h.run(); await h.refresh(); assert.equal(h.calls.length, 0); h.cleanup()
  const hidden = harness(); hidden.doc.visibilityState = "hidden"; await hidden.run(); assert.equal(hidden.calls.length, 0); hidden.cleanup()
})
test("focused chat marks only the measured final reply, not the newer offscreen one", async () => {
  const h = harness(); await h.run()
  assert.deepEqual(h.calls.map(c => c.slice(0, 3)), [["snapshot"], ["read", "signed", "on-screen"]])
  await h.refresh(); assert.equal(h.calls.filter(c => c[0] === "read").length, 1)
  h.cleanup()
})
test("late snapshots cannot advance a foreign workspace or an unmounted reader", async () => {
  for (const unmount of [false, true]) {
    let resolve
    const h = harness({ deferred: new Promise(r => { resolve = r }) }); await h.run()
    if (unmount) h.cleanup(); else h.state.workspace = { id: "ws2" }
    resolve(); await tick()
    assert.deepEqual(h.calls, [["snapshot"]])
    if (!unmount) h.cleanup()
  }
})
test("old Hub unsupported state is not repeatedly requested on every visible check", async () => {
  const h = harness({ failure: { unsupported: true } }); await h.run(); await h.refresh(); await h.refresh()
  assert.deepEqual(h.calls, [["snapshot"]]); h.cleanup()
})
