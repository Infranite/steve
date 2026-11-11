import assert from "node:assert/strict"
import { test } from "node:test"

const m = await import("../src/audit.ts")
const entry = (action, detail) => ({ id: 1, ts: 0, actor: "usr_1", action, target: "default", detail })

test("an action's label key has no dots, so one action can be the start of another", () => {
  assert.equal(m.actionKey("settings.models"), "settings_models")
  assert.equal(m.actionKey("settings.models.migrate"), "settings_models_migrate")
})

test("an action on something of a type may be worded for that type, and goes by its own words otherwise", () => {
  assert.deepEqual(m.actionKeys(entry("party.create", { type: "org", self: true })), ["party_create_org", "party_create"])
  assert.deepEqual(m.actionKeys(entry("party.create", { type: 3 })), ["party_create"])
  assert.deepEqual(m.actionKeys(entry("party.create", { type: "a.b" })), ["party_create"])
  assert.deepEqual(m.actionKeys(entry("bot.create", null)), ["bot_create"])
})

test("a detail's ids go by their names, a model choice by its harness and models, a missing value by a dash", () => {
  const names = { ct_0123456789abcdef: "小李" }
  assert.equal(m.shown("ct_0123456789abcdef", names), "小李")
  assert.equal(m.shown("ct_unknown", names), "ct_unknown")
  assert.equal(m.shown(null), "—")
  assert.equal(m.shown(["name", "note"]), "name, note")
  assert.equal(m.shown({ harness: "h1", models: ["big", "small"], effort: "high" }), "h1 · big, small · high")
  assert.equal(m.shown({ harness: "h1", models: ["big"] }), "h1 · big")
  assert.equal(m.shown({ type: "contact", me: true }), '{"type":"contact","me":true}')
})

test("a before/after pair reads as what changed, one line a key, and names the purposes it touched", () => {
  const e = entry("settings.models", {
    before: { "models.route": null, "models.title": { harness: "h1", models: ["a"] }, "models.v3": null },
    after: { "models.route": { harness: "h1", models: ["quick"] }, "models.title": { harness: "h1", models: ["a"] }, "models.v3": 1 },
  })
  assert.deepEqual(m.changed(e), ["models.route", "models.v3"])
  assert.deepEqual(m.detailLines(e), [["models.route", "— → h1 · quick"], ["models.v3", "— → 1"]])
  assert.deepEqual(m.changed(e).map(m.purposeOf).filter(Boolean), ["route"])
})

test("every other key of a detail is a line; an empty list says nothing; an entry without detail has no lines", () => {
  const names = { ct_0123456789abcdef: "小李" }
  assert.deepEqual(m.detailLines(entry("steward.link", { object_type: "bot", person: "ct_0123456789abcdef" }), names), [["object_type", "bot"], ["person", "小李"]])
  assert.deepEqual(m.detailLines(entry("onboard.bots", { created: ["写文档"], skipped: [] })), [["created", "写文档"]])
  assert.deepEqual(m.detailLines(entry("onboard.start", null)), [])
  assert.deepEqual(m.changed(entry("bot.create", { name: "写文档" })), [])
})
