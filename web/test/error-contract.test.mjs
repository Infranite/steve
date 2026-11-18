import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import { test } from "node:test"
import ts from "typescript"

const code = ts.transpileModule(readFileSync(new URL("../src/api.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
function client(status, body, contentType = "application/json") {
  const exports = {}
  runInNewContext(code, { exports, AbortSignal, URLSearchParams, require: (id) => {
    if (id === "@/i18n") return { lang: () => "en", t: (key) => key }
    if (id === "./stream-cursor") return {}
    if (id === "./hub") return { hubUrl: (path) => path }
    throw new Error(id)
  }, fetch: async () => ({ ok: status < 400, status, headers: { get: (key) => key === "content-type" ? contentType : null }, json: async () => body }) })
  return exports
}

test("session sign-in keeps the legacy unauthenticated marker or login hint", async () => {
  for (const body of [
    { error: "unauthenticated", code: "unauthenticated", retry: { kind: "never" }, reason: null, params: {} },
    { error: "Please sign in", login: "/auth/login", token: true },
  ]) {
    const api = client(401, body)
    await assert.rejects(api.modReq("GET", "/people"), e => e instanceof api.Unauthenticated)
  }
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

test("plain-text unknown routes stay unwired and JSON not-found stays an ordinary error", async () => {
  const unwired = client(404, undefined, "text/plain")
  await assert.rejects(unwired.modReq("GET", "/unknown"), e => e instanceof unwired.NotWired)
  const existing = client(404, { error: "No such person", code: "not_found" })
  await assert.rejects(existing.modReq("GET", "/people/missing"), e => e instanceof existing.HubError && e.code === "not_found")
})
