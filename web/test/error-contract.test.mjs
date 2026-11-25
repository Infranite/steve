import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import { test } from "node:test"
import ts from "typescript"

const code = ts.transpileModule(readFileSync(new URL("../src/api.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
function client(status, body, contentType = "application/json", failure) {
  const exports = {}
  runInNewContext(code, { exports, AbortSignal, URLSearchParams, require: (id) => {
    if (id === "@/i18n") return { lang: () => "en", t: (key) => key }
    if (id === "./i18n/reason.ts") return { reasonText: body => body.error }
    if (id === "./stream-cursor") return {}
    if (id === "./hub") return { hubUrl: (path) => path }
    throw new Error(id)
  }, fetch: async () => {
    if (failure) throw failure
    return { ok: status < 400, status, headers: { get: (key) => key === "content-type" ? contentType : null }, json: async () => body, text: async () => body ?? "" }
  } })
  return exports
}

test("the session sign-in response opens authentication", async () => {
  const api = client(401, { error: "unauthenticated", login: "/auth/dev" })
  await assert.rejects(api.modReq("GET", "/people"), e => e instanceof api.Unauthenticated && e.login === "/auth/dev")
})

test("a module 401 stays an ordinary error even with the generic unauthenticated code", async () => {
  for (const code of [undefined, "unauthenticated"]) {
    const api = client(401, { error: "请重新登录后再登记", ...(code ? { code } : {}) })
    await assert.rejects(api.modReq("POST", "/vault/passkeys", {}), e => !(e instanceof api.Unauthenticated) && e instanceof api.HubError && e.status === 401 && e.code === code && e.message === "请重新登录后再登记")
  }
})

test("module errors preserve code, status and display text", async () => {
  const api = client(409, { error: "Changed elsewhere", code: "conflict", retry: { kind: "never" }, reason: null, params: {} })
  await assert.rejects(api.modReq("PATCH", "/people/p1", {}), e => e instanceof api.HubError && e.status === 409 && e.code === "conflict" && e.message === "Changed elsewhere")
})

test("route and resource 404s are HTTP errors", async () => {
  for (const [body, media] of [[undefined, "text/plain"], [{}, "application/json"]]) {
    const api = client(404, body, media)
    await assert.rejects(api.modReq("GET", "/unknown"), e => e instanceof api.HubError && e.status === 404)
  }
  const existing = client(404, { error: "No such person", code: "not_found" })
  await assert.rejects(existing.modReq("GET", "/people/missing"), e => e instanceof existing.HubError && e.code === "not_found")
})

test("HTML and network failures cannot masquerade as module data", async () => {
  const html = client(200, "<!doctype html>", "text/html")
  for (const method of ["GET", "POST"]) {
    await assert.rejects(html.modReq(method, "/channels"), /shell:http.incomplete/)
  }
  const failed = new TypeError("Failed to fetch")
  await assert.rejects(client(0, null, null, failed).modReq("GET", "/channels"), e => e === failed)
  const empty = client(204, undefined, null)
  await empty.modReq("DELETE", "/channels/id")
})

test("unified transport records x-steve-rev and preserves BotPlacementError", async () => {
  const exports = {}
  let response = { ok: true, status: 200, headers: { get: (k) => k === "content-type" ? "application/json" : k === "x-steve-rev" ? "42" : null }, json: async () => ({ id: "ST-1" }), text: async () => "" }
  runInNewContext(code, { exports, AbortSignal, URLSearchParams, require: (id) => {
    if (id === "@/i18n") return { lang: () => "en", t: (key) => key }
    if (id === "./i18n/reason.ts") return { reasonText: body => body.error }
    if (id === "./stream-cursor") return {}
    if (id === "./hub") return { hubUrl: (path) => path }
    throw new Error(id)
  }, fetch: async () => response })
  const created = await exports.api.newItem({ goal: "hi" })
  assert.equal(exports.revOf(created), 42)
  response = { ok: false, status: 409, headers: { get: (k) => k === "content-type" ? "application/json" : null }, json: async () => ({ error: "No matching node", placement: { reason: "offline", rejected: [] } }), text: async () => "" }
  await assert.rejects(exports.api.newItem({ goal: "hi" }), e => e instanceof exports.BotPlacementError && e.blocked.reason === "offline")
})

test("homeTilde collapses both macOS and Linux home directories", async () => {
  const { homeTilde } = await import("../src/lib/path.ts")
  assert.equal(homeTilde("/Users/example/work/repo"), "~/work/repo")
  assert.equal(homeTilde("/home/example/work/repo"), "~/work/repo")
  assert.equal(homeTilde("/opt/steve/repo"), "/opt/steve/repo")
})
