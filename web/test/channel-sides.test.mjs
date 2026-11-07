import assert from "node:assert/strict"
import { test } from "node:test"
import { botAdapterIds, offers, receiveNote, routesOf, selfSideOf, selfSidesFor, startsOf } from "../src/integrations/sides.ts"

const bot = { id: "feishu_app", identity: "bot", auth: "credential", pairing: null, channel: { mode: "bot", receive: "long_connection", start: [{ kind: "form" }] } }
const me = { id: "feishu_user", identity: "self", auth: "oauth", device: true, pairing: { with: ["feishu_app"], required: false }, channel: { mode: "self", receive: "poll", start: [{ kind: "oauth" }, { kind: "device" }] } }

test("authorizing yourself through a bot needs a side acting as you that pairs with it", () => {
  assert.deepEqual(botAdapterIds([bot, me]), ["feishu_app"])
  assert.deepEqual(selfSidesFor([bot, me], "feishu_app").map((a) => a.id), ["feishu_user"])
  // no side acting as you: nothing to authorize
  assert.equal(selfSideOf([bot]), null)
  // one that pairs with another bot only
  assert.deepEqual(selfSidesFor([bot, { ...me, pairing: { with: ["other_bot"], required: true } }], "feishu_app"), [])
  assert.equal(selfSideOf([bot, me])?.id, "feishu_user")
})

test("the start buttons come from the side's declared start", () => {
  assert.deepEqual(startsOf(me).map((s) => s.kind), ["oauth", "device"])
  const form = { ...me, id: "test_self", channel: { ...me.channel, start: [{ kind: "form" }, { kind: "route", path: "/channels/ext/probe/start", label: "用自己的页面接入" }] } }
  assert.equal(offers(startsOf(form), "device"), false)
  assert.equal(offers(startsOf(form), "form"), true)
  assert.deepEqual(routesOf(startsOf(form)), [{ href: "/api/channels/ext/probe/start", label: "用自己的页面接入" }])
  // never a link off the Hub
  assert.deepEqual(routesOf([{ kind: "route", path: "//evil.example/x", label: "x" }, { kind: "route", path: "https://evil.example", label: "y" }]), [])
  // an adapter without a declaration: its auth
  assert.deepEqual(startsOf({ id: "x", identity: "self", auth: "oauth", device: false }).map((s) => s.kind), ["oauth"])
  assert.deepEqual(startsOf({ id: "y", identity: "self", auth: "credential" }).map((s) => s.kind), ["form"])
})

test("the receiving line follows the declaration", () => {
  assert.equal(receiveNote("poll"), "polling")
  assert.equal(receiveNote("long_connection"), "longConnection")
  assert.equal(receiveNote("webhook"), null)
  assert.equal(receiveNote(undefined), null)
})
