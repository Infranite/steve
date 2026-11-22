import assert from "node:assert/strict"
import { test } from "node:test"
import {
  nodeViews, findNode, nodeStatus, offlineReason, statusText, loadPct, memPct, disks, fillTone, agoText, routeOf, routeLabel, ageOf, clientKind, one,
  hostOf, hostLine, linkRoute, rttOf, upgradeCard, nodeAlerts, UPGRADE_STEPS,
  FILL_WARN, FILL_BAD, CLIENT_STALE_MS, CLIENT_UNUSED_MS, LOCAL_NODE,
} from "../src/device-model.ts"
import { setLang } from "../src/i18n/index.ts"

const NOW = Date.UTC(2026, 10, 17, 8)
const hubDevice = { id: "hub", name: "Studio Mac", roles: ["Hub"], os: "macOS", version: "0.2.112", status: "正常", max_sessions: 32, seen: NOW, created: 0, node_id: LOCAL_NODE, connected: true, running: 3, waiting: 0 }
const node = (id, extra = {}) => ({ node_id: id, name: id, fingerprint: "9E3F5A07", state: "active", linked: true, local: false, last_seen: NOW - 60_000, max_sessions: 6, running: 1, waiting: 0, ...extra })
const view = (extra = {}) => ({ id: "n", name: "n", local: false, state: "active", linked: true, paused: false, running: 0, waiting: 0, max: 4, version: "0.2.112", ...extra })
const run = (phase, extra = {}) => ({ operation_id: "op", version: "0.2.112", from_version: "0.2.109", phase, requested_by: "owner", force: false, created: NOW, updated: NOW, ...extra })

test("the Hub's own Node comes first, joined to its device row only by node_id; duplicates and the Hub row are dropped", () => {
  const devices = [hubDevice, { ...hubDevice, id: "old", roles: ["执行节点"], name: "Build box duplicate", node_id: "build" }]
  const nodes = [node("build", { upgrade: { build: { version: "0.2.109", commit: "abc", target: "x" }, policy: "notify", can: true, reason: "outdated" } }), node("build"), node(LOCAL_NODE, { local: true, name: "hub · local" })]
  const vs = nodeViews(devices, nodes, "0.2.112")
  assert.deepEqual(vs.map((v) => v.id), [LOCAL_NODE, "build"])
  assert.equal(vs[0].name, "Studio Mac", "the local Node is named by its device")
  assert.equal(vs[0].max, 32); assert.equal(vs[0].running, 3); assert.equal(vs[0].version, "0.2.112")
  assert.equal(vs[1].device.id, "old"); assert.equal(vs[1].version, "0.2.109")
  assert.equal(findNode(vs, "old")?.id, "build", "an old device link finds its Node")
  assert.equal(findNode(vs, "hub")?.id, LOCAL_NODE)
  assert.equal(findNode(vs, "missing"), undefined)
})

test("two device rows claiming the local Node leave it unjoined instead of picking one", () => {
  const vs = nodeViews([hubDevice, { ...hubDevice, id: "other" }], [node(LOCAL_NODE, { local: true, name: "local node" })])
  assert.equal(vs[0].name, "local node"); assert.equal(vs[0].device, undefined)
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

test("offline duration and code win over older diagnostics and are localized", () => {
  const n = node("remote", { linked: false, last_seen: NOW - 1000,
    offline: { reason: "protocol_mismatch", since: NOW - 3_600_000, duration_ms: 3_600_000 },
    link_error: { at: NOW - 1000, error: "raw fixture detail" } })
  const [v] = nodeViews([], [n])
  assert.equal(statusText(nodeStatus(v), NOW), "已离线 1 小时")
  assert.equal(offlineReason(v), "协议不兼容，请升级 Hub")
  assert.equal(nodeAlerts(v, "", NOW)[0].text, offlineReason(v))
  try {
    setLang("en")
    assert.match(statusText(nodeStatus(v), NOW), /^Offline for 1 /)
    assert.equal(offlineReason(v), "Incompatible protocols; upgrade the Hub")
    assert.equal(offlineReason({ ...v, linked: true }), "")
  } finally {
    setLang("zh-CN")
  }
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

test("a Node's machine: its reported host first, the Hub's device row for its own, cores and memory from a sample", () => {
  const mac = hostOf(view({ node: node("m", { host: { os: "macos", os_version: "26.1", arch: "aarch64", cpus: 12, mem_total: 36 * 2 ** 30 } }) }))
  assert.deepEqual(mac, { os: "macOS 26.1", kind: "mac", arch: "arm64", cpus: 12, memGb: 36, hostname: null })
  assert.equal(hostLine(mac), "macOS 26.1 · arm64 · 12 核 · 36 GB")
  const linux = hostOf(view({ node: node("l", { host: { os: "linux", os_name: "Ubuntu 24.04 LTS", os_version: "24.04", arch: "x86_64", hostname: "build-01" } }) }), { at: 1, cpus: 8, mem_total: 32 * 2 ** 30 })
  assert.equal(linux.os, "Ubuntu 24.04 LTS", "a name that already says its version is not repeated"); assert.equal(linux.kind, "linux")
  assert.equal(hostLine(linux), "Ubuntu 24.04 LTS · x86_64 · 8 核 · 32 GB")
  const local = hostOf(view({ local: true, device: { ...hubDevice, os: "macOS 26.1 · 64 GB" } }), { at: 1, cpus: 14, mem_total: 64 * 2 ** 30 })
  assert.equal(hostLine(local), "macOS 26.1 · 64 GB · 14 核", "a size the system line names is not repeated")
  assert.equal(hostLine(hostOf(view({ name: "mac-mini" }))), "", "nothing is guessed from a name")
})

test("the route a Node came in on, and its round trip", () => {
  const link = (l) => view({ node: node("n", { link: { route: "lan", tls: false, via: "direct", since: NOW, latency_ms: 38.4, latency_at: NOW, ...l } }) })
  assert.equal(linkRoute(view({ local: true })), "local")
  assert.equal(linkRoute(link({ via: "tailcat", route: "loopback" })), "tailcat")
  assert.equal(linkRoute(link({ via: "proxy", route: "public", tls: true })), "proxy")
  assert.equal(linkRoute(link({})), "lan"); assert.equal(linkRoute(link({ route: "tailnet" })), "tailnet")
  assert.equal(linkRoute(link({ route: "public", tls: true })), "https"); assert.equal(linkRoute(link({ route: "public" })), "public")
  assert.equal(linkRoute(link({ route: "unknown" })), null); assert.equal(linkRoute(view({ node: node("n") })), null, "an older Hub says nothing")
  assert.equal(rttOf(link({})), 38); assert.equal(rttOf(view({ node: node("n") }), { at: 1, rtt_ms: 61.6 }), 62)
  assert.equal(rttOf(view({ local: true }), { at: 1, rtt_ms: 1 }), null, "the Hub's own Node has no round trip")
})

test("the version card: one state per Node, and only a rollback claims a failed step", () => {
  const up = (u, extra = {}) => view({ node: node("n", { upgrade: { policy: "notify", can: false, reason: "current", ...u } }), ...extra })
  assert.deepEqual(upgradeCard(view({ local: true })), { kind: "builtIn" })
  assert.deepEqual(upgradeCard(view({ node: node("n") })), { kind: "none" }, "an older Hub: no card")
  const running = upgradeCard(up({ latest: run("draining") }), "0.2.112")
  assert.equal(running.kind, "running"); assert.deepEqual(running.steps, ["done", "done", "active", "todo", "todo"]); assert.equal(UPGRADE_STEPS.length, 5)
  assert.deepEqual(upgradeCard(up({ latest: run("staged") })).steps, ["done", "done", "active", "todo", "todo"], "downloaded: next it waits for running work")
  assert.deepEqual(upgradeCard(up({ latest: run("switching") }, { linked: false })).steps, ["done", "done", "done", "active", "todo"], "a switch shows even while the link is down")
  assert.deepEqual(upgradeCard(up({ reason: "unknown" }, { linked: false, version: "0.2.110" })), { kind: "offline", last: "0.2.110" })
  const back = upgradeCard(up({ reason: "outdated", can: true, latest: run("rolled_back", { error: "no link" }) }, { version: "0.2.109" }), "0.2.112")
  assert.equal(back.kind, "failed"); assert.deepEqual(back.steps, ["done", "done", "done", "bad", "back"])
  assert.equal(upgradeCard(up({ reason: "outdated", can: true, latest: run("failed") }, { version: "0.2.109" }), "0.2.112").steps, null)
  assert.deepEqual(upgradeCard(up({ reason: "outdated", can: true }, { version: "0.2.109" }), "0.2.112"), { kind: "outdated", from: "0.2.109", to: "0.2.112" })
  assert.deepEqual(upgradeCard(up({ reason: "manual" }, { version: "0.2.109" }), "0.2.112"), { kind: "manual", from: "0.2.109", to: "0.2.112", behind: true })
  assert.equal(upgradeCard(up({ reason: "manual" }), "0.2.112").behind, false)
  assert.deepEqual(upgradeCard(up({ latest: run("succeeded") })), { kind: "current", last: run("succeeded") })
  assert.equal(upgradeCard(up({ reason: "hub_dev_build" })).kind, "devHub")
})

test("a card leads with at most two lines, what went wrong first", () => {
  const up = (u, extra = {}) => view({ node: node("n", { upgrade: { policy: "notify", can: true, reason: "outdated", ...u } }), version: "0.2.109", ...extra })
  assert.deepEqual(nodeAlerts(view(), "0.2.112"), [], "nothing to say: nothing shown")
  assert.deepEqual(nodeAlerts(up({ latest: run("rolled_back") }), "0.2.112").map((a) => [a.tone, a.kind]), [["bad", "rolledBack"]], "the failure says it all")
  assert.match(nodeAlerts(up({ latest: run("rolled_back") }), "0.2.112")[0].text, /升级到 0\.2\.112 没有成功，已自动回到 0\.2\.109/)
  assert.deepEqual(nodeAlerts(up({ reason: "manual", can: false }), "0.2.112").map((a) => a.kind), ["manual"])
  assert.match(nodeAlerts(up({ reason: "manual", can: false }), "0.2.112")[0].text, /steve node update/)
  const off = nodeAlerts(view({ linked: false, linkError: { at: NOW - 3 * 3_600_000, error: "connection refused: 7701" } }), "", NOW)
  assert.deepEqual(off.map((a) => a.kind), ["offline"]); assert.match(off[0].text, /3 小时前.*connection refused/)
  assert.deepEqual(nodeAlerts(view({ state: "pending" })).map((a) => a.kind), ["pending"])
  assert.deepEqual(nodeAlerts(up({ latest: run("draining") }), "0.2.112"), [], "an upgrade under way is the badge's to say")
})
