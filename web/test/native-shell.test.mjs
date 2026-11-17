import assert from "node:assert/strict"
import { test } from "node:test"
import { NativeShellSession, shellBridge, nativeHubChrome, shellRoute, shellDestination } from "../src/native-shell.ts"

const caps = ["shell.context", "shell.navigation.next", "shell.navigation.ack"]
const context = (patch = {}) => ({ workspaceID: "first", workspaceName: "First", route: "#/", title: "Steve", ready: true, authenticated: true, focused: true, visible: true, ...patch })
function fixture(intent, overrides = {}, portOverrides = {}) {
  const calls = [], navigated = [], selected = []
  let current = context(), action = false
  const bridge = { v: 2, platform: "macos", can: caps, call: async (op, args) => {
    calls.push([op, args])
    if (op === "shell.navigation.next") return intent
    if (op === "hubs.list") return { hubs: [{ id: "hub-a", current: true }] }
    return {}
  }, ...overrides }
  const session = new NativeShellSession(bridge, {
    allowed: () => true,
    hasWorkspace: (id) => ["first", "second"].includes(id),
    selectWorkspace: async (id) => { selected.push(id); current = context({ workspaceID: id, ready: false }); session.update(current); await Promise.resolve(); current = { ...current, ready: true }; session.update(current); return true },
    navigate: (route) => { navigated.push(route); action = route.startsWith("#/hub/"); current = { ...current, route: shellDestination(route) }; session.update(current) },
    actionOpen: () => action,
    ...portOverrides,
  })
  session.update(current)
  return { session, bridge, calls, navigated, selected, update: (patch) => { current = { ...current, ...patch }; session.update(current) } }
}
const navigation = (patch = {}) => ({ id: "intent-1", hubID: "hub-a", route: "#/inbox", ...patch })

test("browser/old native and iOS keep Hub chrome unless Mac explicitly replaces it", () => {
  assert.equal(shellBridge(null), null)
  assert.equal(shellBridge({ can: ["hubs.list"] }), null)
  assert.equal(nativeHubChrome({ platform: "macos", can: caps }), false)
  assert.equal(nativeHubChrome({ platform: "macos", can: [...caps, "shell.chrome.hub"] }), true)
  assert.equal(nativeHubChrome({ platform: "ios", can: [...caps, "shell.chrome.hub"] }), false)
})
test("route allowlist rejects scripts, external/unknown routes, traversal and code-bearing pairing links", () => {
  for (const route of ["javascript:bad", "https://example.test", "#//settings", "#/items/%2e%2e/settings", "#/items/%5c", "#/items/%00", "#/items/%23", "#/unknown", "#/hub/delete", "#/hub/pair?code=secret", "#/pair?code=secret", "#/auto/acc/old"]) assert.equal(shellRoute(route), null, route)
  assert.equal(shellRoute("/inbox"), "#/inbox")
  assert.equal(shellRoute("#/settings/hub"), "#/settings/hub")
})
test("context coalesces by metadata, nullable workspace and focus are preserved", async () => {
  const f = fixture(null)
  try {
    await f.session.publish()
    f.update({}); f.update({}); await f.session.publish()
    assert.equal(f.calls.filter(([op]) => op === "shell.context").length, 1)
    f.update({ focused: false, visible: false }); await f.session.publish()
    assert.equal(f.calls.filter(([op]) => op === "shell.context").length, 2)
    f.update({ ready: false, authenticated: false, workspaceID: null, workspaceName: null }); await f.session.publish()
    assert.equal(f.calls.at(-1)[1].workspaceID, null)
    await f.session.poll()
    assert.equal(f.calls.filter(([op]) => op === "shell.navigation.next").length, 0)
  } finally { f.session.stop() }
})
test("next is a direct intent, context reaches scoped destination before success ack", async () => {
  const f = fixture(navigation({ workspaceID: "second" }))
  try {
    await f.session.poll()
    assert.deepEqual(f.selected, ["second"])
    assert.deepEqual(f.navigated, ["#/inbox"])
    assert.deepEqual(f.calls.at(-1), ["shell.navigation.ack", { id: "intent-1", ok: true }])
    const published = f.calls.filter(([op]) => op === "shell.context").at(-1)[1]
    assert.equal(published.workspaceID, "second")
    assert.equal(published.route, "#/inbox")
  } finally { f.session.stop() }
})
test("modal routes acknowledge actual opened action at #/, settings aliases acknowledge canonical destination", async () => {
  for (const route of ["#/hub/add", "#/hub/pair", "#/settings/hub", "#/settings/access"]) {
    const f = fixture(navigation({ route }))
    try { await f.session.poll(); assert.deepEqual(f.calls.at(-1), ["shell.navigation.ack", { id: "intent-1", ok: true }], route) }
    finally { f.session.stop() }
  }
})
test("unsupported target, unknown workspace and wrong Hub produce honest failures", async () => {
  for (const patch of [{ targetID: "reply-1" }, { workspaceID: "missing" }, { hubID: "hub-b" }, { route: "javascript:bad" }]) {
    const f = fixture(navigation(patch))
    try {
      await f.session.poll()
      assert.equal(f.calls.at(-1)[0], "shell.navigation.ack")
      assert.equal(f.calls.at(-1)[1].ok, false)
      assert.equal(typeof f.calls.at(-1)[1].error, "string")
      assert.deepEqual(f.navigated, [])
    } finally { f.session.stop() }
  }
})
test("edit/in-flight guard rejects cross-workspace intent before any selection or navigation", async () => {
  const f = fixture(navigation({ workspaceID: "second" }), {}, { allowed: () => false })
  try {
    await f.session.poll()
    assert.deepEqual(f.selected, [])
    assert.deepEqual(f.navigated, [])
    assert.equal(f.calls.at(-1)[0], "shell.navigation.ack")
    assert.equal(f.calls.at(-1)[1].ok, false)
    assert.match(f.calls.at(-1)[1].error, /active edit/)
  } finally { f.session.stop() }
})
test("Hub switch approval uses the current guard even on a not-ready attached page", async () => {
  for (const approved of [false, true]) {
    const calls = [], order = []
    const f = fixture(null, {
      can: [...caps, "shell.hub-switch.next", "shell.hub-switch.ack"],
      call: async (op, args) => {
        calls.push([op, args])
        if (op === "shell.hub-switch.next") return { id: "switch-1", hubID: "hub-b" }
        return {}
      },
    }, { allowed: () => { order.push("guard"); return approved } })
    try {
      f.update({ ready: false })
      await f.session.poll()
      assert.deepEqual(order, ["guard"])
      assert.deepEqual(calls.at(-1), ["shell.hub-switch.ack", { id: "switch-1", approved }])
      assert.equal(calls.some(([op]) => op === "shell.navigation.next"), false)
      assert.deepEqual(f.selected, [])
      assert.deepEqual(f.navigated, [])
    } finally { f.session.stop() }
  }
})
test("stale Hub switch response does not ask the guard or approve for a replacement scope", async () => {
  let resolve
  const calls = [], guards = []
  const f = fixture(null, {
    can: [...caps, "shell.hub-switch.next", "shell.hub-switch.ack"],
    call: async (op, args) => {
      calls.push([op, args])
      if (op === "shell.hub-switch.next") return new Promise((r) => { resolve = r })
      return {}
    },
  }, { allowed: () => { guards.push(true); return true } })
  try {
    const pending = f.session.poll()
    while (!resolve) await new Promise((r) => setImmediate(r))
    f.update({ authenticated: false, workspaceID: null, workspaceName: null })
    resolve({ id: "stale", hubID: "hub-b" })
    await pending
    assert.deepEqual(guards, [])
    assert.equal(calls.some(([op]) => op === "shell.hub-switch.ack"), false)
  } finally { f.session.stop() }
})
test("pending next never overlaps; logout and dispose cancel a late result without ack or route", async () => {
  for (const cancel of ["logout", "dispose", "workspace"]) {
    let resolve, nextCalls = 0
    const f = fixture(null, { call: async (op) => { if (op === "shell.navigation.next") { nextCalls++; return new Promise((r) => { resolve = r }) } return {} } })
    try {
      const pending = f.session.poll()
      while (!resolve) await new Promise((r) => setImmediate(r))
      await f.session.poll()
      assert.equal(nextCalls, 1)
      if (cancel === "logout") f.update({ authenticated: false, ready: false, workspaceID: null })
      if (cancel === "dispose") f.session.stop()
      if (cancel === "workspace") f.update({ workspaceID: "second" })
      resolve(navigation())
      await pending
      assert.deepEqual(f.navigated, [])
      assert.equal(f.calls.some(([op]) => op === "shell.navigation.ack"), false)
    } finally { f.session.stop() }
  }
})
