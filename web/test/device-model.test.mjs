import assert from "node:assert/strict"
import { test } from "node:test"
import {
  nodeViews, findNode, nodeStatus, statusText, loadPct, memPct, disks, fillTone, agoText, routeOf, routeLabel, ageOf, clientKind, one,
  FILL_WARN, FILL_BAD, CLIENT_STALE_MS, CLIENT_UNUSED_MS, LOCAL_NODE,
} from "../src/device-model.ts"

const NOW = Date.UTC(2026, 10, 17, 8)
const hubDevice = { id: "hub", name: "Studio Mac", roles: ["Hub"], os: "macOS", version: "0.2.112", status: "正常", max_sessions: 32, seen: NOW, created: 0, node_id: LOCAL_NODE, connected: true, running: 3, waiting: 0 }
const node = (id, extra = {}) => ({ node_id: id, name: id, fingerprint: "9E3F5A07", state: "active", linked: true, local: false, last_seen: NOW - 60_000, max_sessions: 6, running: 1, waiting: 0, ...extra })
const view = (extra = {}) => ({ id: "n", name: "n", local: false, legacy: false, state: "active", linked: true, paused: false, running: 0, waiting: 0, max: 4, version: "0.2.112", ...extra })
const run = (phase, extra = {}) => ({ operation_id: "op", version: "0.2.112", from_version: "0.2.109", phase, requested_by: "owner", force: false, created: NOW, updated: NOW, ...extra })

test("the Hub's own Node comes first, joined to its device row only by node_id; duplicates and the Hub row are dropped", () => {
  const devices = [hubDevice, { ...hubDevice, id: "old", roles: ["执行节点"], name: "Build box duplicate", node_id: "build" }, { ...hubDevice, id: "legacy", roles: ["执行节点"], name: "Old runner", node_id: null, connected: null }]
  const nodes = [node("build", { upgrade: { build: { version: "0.2.109", commit: "abc", target: "x" }, policy: "notify", can: true, reason: "outdated" } }), node("build"), node(LOCAL_NODE, { local: true, name: "hub · local" })]
  const vs = nodeViews(devices, nodes, "0.2.112")
  assert.deepEqual(vs.map((v) => v.id), [LOCAL_NODE, "build", "legacy"])
  assert.equal(vs[0].name, "Studio Mac", "the local Node is named by its device")
  assert.equal(vs[0].max, 32); assert.equal(vs[0].running, 3); assert.equal(vs[0].version, "0.2.112")
  assert.equal(vs[1].device.id, "old"); assert.equal(vs[1].version, "0.2.109")
  assert.equal(vs[2].legacy, true); assert.equal(vs[2].linked, false)
  assert.equal(findNode(vs, "old")?.id, "build", "an old device link finds its Node")
  assert.equal(findNode(vs, "hub")?.id, LOCAL_NODE)
  assert.equal(findNode(vs, "missing"), undefined)
})

test("two device rows claiming the local Node leave it unjoined instead of picking one", () => {
  const vs = nodeViews([hubDevice, { ...hubDevice, id: "other" }], [node(LOCAL_NODE, { local: true, name: "local node" })])
  assert.equal(vs[0].name, "local node"); assert.equal(vs[0].device, undefined)
})

test("while the registry is unread, device rows that name a Node are not listed as legacy devices", () => {
  const vs = nodeViews([hubDevice, { ...hubDevice, id: "x", roles: [], node_id: "remote" }], null)
  assert.deepEqual(vs.map((v) => v.id), [LOCAL_NODE])
})

test("one status per Node, the first that holds in the fixed order", () => {
  const latest = (phase, extra) => ({ node: { upgrade: { policy: "notify", can: true, reason: "outdated", latest: run(phase, extra) } } })
  const cases = [
    [view({ state: "revoked", linked: false }), "revoked", "off"],
    [view({ state: "pending", linked: false }), "pending", "warn"],
    [view({ linked: false, lastSeen: NOW - 3 * 3_600_000, ...latest("draining") }), "offline", "off"],
    [view({ paused: true, waiting: 2, ...latest("draining") }), "upgrading", "info"],
    [view({ paused: true, version: "0.2.109", ...latest("rolled_back") }), "rolledBack", "bad"],
    [view({ paused: true, waiting: 2, version: "0.2.109", node: { upgrade: { policy: "notify", can: true, reason: "outdated" } } }), "paused", "neutral"],
    [view({ waiting: 2, version: "0.2.109", node: { upgrade: { policy: "notify", can: true, reason: "outdated" } } }), "outdated", "warn"],
    [view({ waiting: 2 }), "queued", "warn"],
    [view({}), "online", "ok"],
    [view({ version: "0.2.112", node: { upgrade: { policy: "notify", can: false, reason: "current", latest: run("rolled_back") } } }), "online", "ok"],
  ]
  for (const [v, kind, tone] of cases) assert.deepEqual([nodeStatus(v, "0.2.112").kind, nodeStatus(v, "0.2.112").tone], [kind, tone], kind)
  assert.equal(nodeStatus(view({ version: "0.2.110" }), "0.2.112").kind, "outdated", "a version reported without an upgrade report still differs")
  assert.equal(nodeStatus(view({ local: true, version: "0.2.110" }), "0.2.112").kind, "online", "the Hub's own Node is the Hub's build")
  assert.equal(nodeStatus(view({ waiting: 2 })).n, 2)
})

test("a failed upgrade stops showing once it no longer describes the Node", () => {
  const failed = (extra = {}) => ({ node: { upgrade: { policy: "notify", can: true, reason: "outdated", latest: run("rolled_back", extra) } } })
  assert.equal(nodeStatus(view({ version: "0.2.109", ...failed() }), "0.2.112").kind, "rolledBack", "still on the version it fell back to, aiming at the Hub's release")
  assert.equal(nodeStatus(view({ version: "0.2.109", ...failed({ phase: "failed" }) }), "0.2.112").kind, "rolledBack")
  assert.equal(nodeStatus(view({ version: "0.2.109", ...failed() })).kind, "rolledBack", "a development Hub without a version does not retire it")
  const reinstalled = view({ version: "0.2.113", node: { upgrade: { policy: "notify", can: false, reason: "current", latest: run("rolled_back", { version: "0.2.112", from_version: "0.2.111" }) } } })
  assert.equal(nodeStatus(reinstalled, "0.2.113").kind, "online", "reinstalled by hand past the failed target: no stale failure")
  const reinstalledBehind = view({ version: "0.2.111", ...failed({ version: "0.2.113", from_version: "0.2.110" }) })
  assert.equal(nodeStatus(reinstalledBehind, "0.2.113").kind, "outdated", "moved off the fallback version but still behind: it can upgrade")
  const hubMovedOn = view({ version: "0.2.109", ...failed() })
  assert.deepEqual([nodeStatus(hubMovedOn, "0.2.115").kind, nodeStatus(hubMovedOn, "0.2.115").version], ["outdated", "0.2.115"], "the Hub moved on: the new release is offered, not the old failure")
  assert.equal(nodeStatus(view({ version: "0.2.109", paused: true, ...failed() }), "0.2.115").kind, "paused", "falls through in order")
  assert.equal(nodeStatus(view({ version: "0.2.109", ...failed({ from_version: null }) }), "0.2.112").kind, "rolledBack", "an unknown starting version keeps the failure while it aims at the Hub's release")
})

test("statuses read as plain words", () => {
  assert.equal(statusText({ kind: "offline", tone: "off", since: NOW - 3 * 3_600_000 }, NOW), "离线 · 3 小时前")
  assert.equal(statusText({ kind: "outdated", tone: "warn", version: "0.2.112" }), "可升级到 0.2.112")
  assert.equal(statusText({ kind: "queued", tone: "warn", n: 2 }), "排队 2")
  assert.equal(statusText({ kind: "pending", tone: "warn" }), "待确认指纹")
  assert.equal(statusText({ kind: "upgrading", tone: "info", phase: "draining" }), "升级中 · 等运行中的工作结束")
})

test("CPU load is the 1-minute average over the cores; memory and disk are whole figures", () => {
  const l = { at: NOW, cpus: 14, load: [5.88623046875, 4, 3], mem_total: 64 * 2 ** 30, mem_available: 37.76 * 2 ** 30, roots: [{ root: "/a", display: "~/Code", free: 212 * 2 ** 30, total: 994 * 2 ** 30 }, { root: "/b", free: 1, total: 2 ** 30 }, { root: "/bad", free: 5, total: 0 }] }
  assert.equal(loadPct(l), 42); assert.equal(one(l.load[0]), 5.9)
  assert.equal(memPct(l), 41)
  assert.deepEqual(disks(l).map((d) => [d.name, d.free, d.total]), [["/b", 0, 1], ["~/Code", 212, 994]])
  assert.equal(loadPct({ at: NOW, load: [1, 1, 1] }), null, "no cores, no percentage")
  assert.equal(memPct({ at: NOW, mem_total: 10, mem_available: 11 }), null)
  assert.equal(loadPct(null), null)
})

test("fills turn amber and red at named thresholds", () => {
  assert.equal(fillTone(FILL_WARN - 0.01), undefined)
  assert.equal(fillTone(FILL_WARN), "wait")
  assert.equal(fillTone(FILL_BAD), "bad")
  assert.equal(fillTone(null), undefined)
})

test("relative times: 45 seconds is now, then minutes, hours and days", () => {
  assert.equal(agoText(NOW - 44_000, NOW), "刚刚")
  assert.equal(agoText(NOW - 46_000, NOW), "1 分钟前")
  assert.equal(agoText(NOW - 22 * 60_000, NOW), "22 分钟前")
  assert.equal(agoText(NOW - 3 * 3_600_000, NOW), "3 小时前")
  assert.equal(agoText(NOW - 46 * 86_400_000, NOW), "46 天前")
})

test("routes read as words, TLS kept", () => {
  assert.deepEqual(routeOf("tailnet+tls"), { key: "tailnet", raw: "tailnet", tls: true })
  assert.equal(routeLabel("loopback"), "本机")
  assert.equal(routeLabel("public+tls"), "公网 · TLS")
  assert.equal(routeLabel("proxy"), "反向代理")
  assert.equal(routeLabel("carrier-pigeon"), "carrier-pigeon")
  assert.equal(routeLabel(null), "未知")
})

test("clients dim after a week and are flagged after thirty days", () => {
  assert.equal(ageOf(NOW - CLIENT_STALE_MS, 0, NOW), "fresh")
  assert.equal(ageOf(NOW - CLIENT_STALE_MS - 1, 0, NOW), "stale")
  assert.equal(ageOf(NOW - CLIENT_UNUSED_MS - 1, 0, NOW), "unused")
  assert.equal(ageOf(null, NOW - 1000, NOW), "fresh", "never seen: counted from when it signed in")
  assert.equal(CLIENT_STALE_MS, 7 * 86_400_000); assert.equal(CLIENT_UNUSED_MS, 30 * 86_400_000)
})

test("client kinds", () => {
  assert.equal(clientKind("web", "macOS"), "browser")
  assert.equal(clientKind("web", "iOS"), "phone")
  assert.equal(clientKind("ios"), "phone")
  assert.equal(clientKind("mac"), "mac")
  assert.equal(clientKind("cli"), "cli")
  assert.equal(clientKind("watch"), "other")
})
