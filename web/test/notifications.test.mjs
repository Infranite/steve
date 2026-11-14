import assert from "node:assert/strict"
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { runInNewContext } from "node:vm"
import ts from "typescript"

const calls = [], exports = {}
let hub = "https://first.example"
const code = ts.transpileModule(readFileSync(new URL("../src/api/notifications.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
runInNewContext(code, { exports, Intl, require: (id) => {
  if (id === "@/hub") return { currentHub: () => hub }
  assert.equal(id, "@/api")
  return { modReq: async (...args) => { calls.push(args); return { ok: true } } }
} })
const { minuteTime, timeMinute, validQuiet, validTimezone, notificationsApi } = exports

test("quiet hours round-trip local minutes, including midnight and overnight intervals", () => {
  for (let minute = 0; minute < 1440; minute++) assert.equal(timeMinute(minuteTime(minute)), minute)
  assert.equal(minuteTime(null), null)
  assert.equal(timeMinute(null), null)
  for (const invalid of ["24:00", "12:60", "-1:00", "12", "oops"]) assert.equal(timeMinute(invalid), null)
  for (const [start, end, expected] of [[1320, 420, true], [0, 1, true], [null, null, true], [null, 20, false], [20, null, false], [0, 0, false], [-1, 1, false], [0, 1440, false], [0.5, 1, false]]) assert.equal(validQuiet(start, end), expected)
})
test("time zones accept IANA zones, not offsets or an empty/unknown zone", () => {
  for (const zone of ["UTC", "Asia/Shanghai", "Europe/London", "America/New_York"]) assert.equal(validTimezone(zone), true)
  for (const zone of ["", " ", "Mars/Olympus", "+08:00"]) assert.equal(validTimezone(zone), false)
})
test("personal notification API uses caller-scoped routes and the complete PUT body", async () => {
  calls.length = 0
  const api = notificationsApi.forWorkspace("workspace-a")
  hub = "https://second.example"
  const policy = { enabled: true, mode: "now", party_id: "self-party", space_id: "bot-dm", timezone: "Asia/Shanghai", quiet_start: 1320, quiet_end: 420 }
  await api.me(); await api.policy(); await api.targets(); await api.inbox(); await api.save(policy); await api.seen("id/with space")
  assert.deepEqual(JSON.parse(JSON.stringify(calls.map(([method, path, body]) => body === undefined ? [method, path] : [method, path, body]))), [["GET", "/me/contact"], ["GET", "/me/notification-policy"], ["GET", "/me/notification-targets"], ["GET", "/me/notifications"], ["PUT", "/me/notification-policy", policy], ["POST", "/me/notifications/id%2Fwith%20space/seen", {}]])
  for (const call of calls) assert.deepEqual(JSON.parse(JSON.stringify(call[4])), { workspace: "workspace-a", hub: "https://first.example" })
})


test("module requests bind explicit workspace and Hub without changing the default caller", async () => {
  const exports = {}, requests = []
  let destination = "https://current.example"
  const code = ts.transpileModule(readFileSync(new URL("../src/api.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 } }).outputText
  runInNewContext(code, { exports, AbortSignal, URLSearchParams, require: (id) => {
    if (id === "@/i18n") return { lang: () => "en", t: (key) => key }
    if (id === "./stream-cursor") return {}
    if (id === "./hub") return { hubUrl: (path) => destination + path }
    throw new Error(id)
  }, fetch: async (url, options) => { requests.push({ url, options }); return { ok: true, status: 200, headers: { get: () => "application/json" }, json: async () => ({ ok: true }) } } })
  exports.setPageWorkspace("workspace-b")
  const scope = { workspace: "workspace-a", hub: "https://original.example" }
  await exports.modReq("PUT", "/me/notification-policy", { enabled: false }, undefined, scope)
  await exports.modReq("POST", "/me/notifications/id/seen", {}, undefined, scope)
  await exports.modReq("GET", "/me/notification-targets", undefined, undefined, scope)
  destination = "https://changed.example"
  await exports.modReq("GET", "/me/notification-policy")
  for (const request of requests.slice(0, 3)) {
    assert.ok(request.url.startsWith("https://original.example/api/"))
    assert.equal(request.options.headers["x-steve-ws"], "workspace-a")
    assert.equal(request.options.credentials, "include")
    assert.equal(request.options.headers["accept-language"], "en")
  }
  assert.equal(requests[0].options.headers["content-type"], "application/json")
  assert.equal(requests[3].url, "https://changed.example/api/me/notification-policy")
  assert.equal(requests[3].options.headers["x-steve-ws"], "workspace-b")
})
