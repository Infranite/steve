import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import { test } from "node:test"
import ts from "typescript"
import i18n, * as ui from "../src/i18n/index.ts"
import { reasonText } from "../src/i18n/reason.ts"
import { date, msDur } from "../src/i18n/format.ts"
import { REASONS } from "../src/gen/reasons.ts"
import zh from "../src/i18n/locales/zh-CN/reason.ts"
import en from "../src/i18n/locales/en/reason.ts"

const PLURAL = /_(zero|one|two|few|many|other)$/
const leaves = (obj, prefix = "") => Object.entries(obj).flatMap(([k, v]) =>
  v && typeof v === "object" ? leaves(v, `${prefix}${k}.`) : [[prefix + k, v]])

function checkTexts(registry, texts) {
  const used = new Set()
  for (const [key, value] of leaves(texts)) {
    const reason = Object.hasOwn(registry, key) ? key : key.replace(PLURAL, "")
    assert.ok(Object.hasOwn(registry, reason), `unregistered translation: ${key}`)
    assert.equal(typeof value, "string", key)
    assert.ok(value.trim(), `empty translation: ${key}`)
    used.add(reason)
    for (const [, param] of value.matchAll(/\{\{-?\s*([\w.]+)[^}]*\}\}/g)) {
      assert.ok(registry[reason].params.includes(param), `${key}: undeclared parameter ${param}`)
    }
    if (reason !== key) assert.ok(registry[reason].params.includes("count"), `${key}: plural needs count`)
  }
  assert.deepEqual([...used].sort(), Object.keys(registry).sort(), "missing reason translations")
}

test("every registered reason has texts, with only declared parameters and no retired keys", () => {
  checkTexts(REASONS, zh)
  checkTexts(REASONS, en)
})

test("coverage rejects missing, unregistered and retired keys and undeclared parameters", () => {
  const registry = { "test.moved": { code: "stale", params: ["branch"] } }
  checkTexts(registry, { test: { moved: "Branch {{branch}} moved" } })
  assert.throws(() => checkTexts(registry, {}), /missing/)
  assert.throws(() => checkTexts(registry, { test: { old: "Retired" } }), /unregistered/)
  assert.throws(() => checkTexts(registry, { test: { moved: "{{now}}" } }), /undeclared/)
  checkTexts({ "test.items": { params: ["count"] } }, { test: { items_one: "{{count}} item", items_other: "{{count}} items" } })
})

const source = ts.transpileModule(readFileSync(new URL("../src/api.ts", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText
function client(status, body) {
  const exports = {}
  runInNewContext(source, { exports, AbortSignal, URLSearchParams, require: (id) => {
    if (id === "@/i18n") return ui
    if (id === "./stream-cursor") return {}
    if (id === "./hub") return { hubUrl: path => path }
    throw new Error(id)
  }, fetch: async () => ({ ok: false, status, headers: { get: k => k === "content-type" ? "application/json" : null }, json: async () => body }) })
  return exports
}

test("absent, unknown, malformed and untranslated reasons keep the safe detail and HTTP status", async () => {
  for (const reason of [undefined, null, "future.new", "Malformed reason", "", "test.missing"]) {
    for (const status of [400, 409]) {
      const body = { error: "Safe detail", code: status === 400 ? "invalid" : "conflict", reason, params: {} }
      assert.equal(reasonText(body), body.error)
      const api = client(status, body)
      await assert.rejects(api.modReq("POST", "/test", {}), e =>
        e instanceof api.HubError && e.message === body.error && e.status === status && e.code === body.code && e.reason === reason && e.params === body.params)
    }
  }
})

test("reason texts format dates and durations, preserve numeric count and flow through HubError", async () => {
  const registry = {
    "test.moved": { code: "stale", params: ["branch", "ready_at", "wait_ms"] },
    "test.items": { code: "busy", params: ["count"] },
    "test.missing": { code: "invalid", params: [] },
  }
  Object.assign(REASONS, registry)
  i18n.addResourceBundle("en", "reason", { test: {
    moved: "{{branch}} at {{ready_at}} after {{wait_ms}}",
    items_one: "{{count}} item", items_other: "{{count}} items",
    unregistered: "Must not be displayed",
  } }, true, true)
  const oldLanguage = ui.lang()
  try {
    await i18n.changeLanguage("en")
    const params = { branch: "main", ready_at: 1_800_000_000_000, wait_ms: 1500 }
    const body = { error: "Fallback", code: "stale", reason: "test.moved", params }
    const expected = `main at ${date(params.ready_at, { dateStyle: "medium", timeStyle: "short" })} after ${msDur(params.wait_ms)}`
    assert.equal(reasonText(body), expected)
    assert.equal(params.wait_ms, 1500)
    assert.equal(reasonText({ error: "Fallback", reason: "test.items", params: { count: 1 } }), "1 item")
    assert.equal(reasonText({ error: "Fallback", reason: "test.items", params: { count: 2 } }), "2 items")
    for (const reason of ["test.missing", "test.unregistered"]) assert.equal(reasonText({ error: "Fallback", reason }), "Fallback")
    const api = client(409, body)
    await assert.rejects(api.modReq("PATCH", "/test", {}), e => e instanceof api.HubError && e.message === expected && e.code === "stale" && e.reason === body.reason && e.params === params)
  } finally {
    for (const key of Object.keys(registry)) delete REASONS[key]
    i18n.removeResourceBundle("en", "reason")
    i18n.addResourceBundle("en", "reason", en)
    await i18n.changeLanguage(oldLanguage)
  }
})
