import assert from "node:assert/strict"
import { test } from "node:test"
import {
  edgeOf, edgeWidth, edgeFlows, SELF_CLIENT, mapRank, mapNodes, keepOrder, clientOnline, mapClients, hCurve, vCurve, wideEdges, narrowEdges,
  MAP_COMPACT_FROM, MAP_MAX_NODES, MAP_MAX_CLIENTS,
} from "../src/device-map-model.ts"
import { fpDigits, fpGroups, fpCheck, fpSpread, nodeStatus } from "../src/device-model.ts"

const NOW = Date.UTC(2026, 10, 17, 8)
const view = (id, extra = {}) => ({ id, name: id, local: false, state: "active", linked: true, paused: false, running: 0, waiting: 0, max: 4, version: "0.2.112", ...extra })
const run = (phase, extra = {}) => ({ operation_id: "op", version: "0.2.112", from_version: "0.2.109", phase, requested_by: "owner", force: false, created: NOW, updated: NOW, ...extra })
const client = (id, extra = {}) => ({ id, kind: "web", name: id, platform: "macOS", version: "x", origin: null, created: NOW - 100 * 86_400_000, last_seen: NOW - 60_000, last_route: "lan", last_ip: null, revoked: null, ...extra })

test("each status draws its own edge, told apart by style as well as color", () => {
  const kinds = {
    pending: view("p", { state: "pending" }), off: view("o", { linked: false }), upgrading: view("u", { node: { upgrade: { policy: "notify", can: true, reason: "outdated", latest: run("draining") } } }),
    bad: view("b", { version: "0.2.109", node: { upgrade: { policy: "notify", can: true, reason: "outdated", latest: run("rolled_back") } } }),
    neutral: view("n", { paused: true }), warn: view("q", { waiting: 2 }), ok: view("k"),
  }
  for (const [want, v] of Object.entries(kinds)) assert.equal(edgeOf(nodeStatus(v, "0.2.112")), want, want)
  assert.equal(edgeOf({ kind: "outdated", tone: "warn" }), "warn")
  assert.equal(edgeOf({ kind: "revoked", tone: "off" }), "off")
})

test("an edge grows with its sessions up to a cap, is thin to a Node away, and flows only while sessions run", () => {
  assert.equal(edgeWidth({ linked: true, running: 0 }), 1.6)
  assert.equal(edgeWidth({ linked: true, running: 2 }), 2.5)
  assert.equal(edgeWidth({ linked: true, running: 50 }), 4)
  assert.equal(edgeWidth({ linked: false, running: 3 }), 1.5)
  assert.equal(edgeFlows({ linked: true, running: 1 }, "ok"), true)
  assert.equal(edgeFlows({ linked: true, running: 0 }, "ok"), false)
  assert.equal(edgeFlows({ linked: true, running: 3 }, "warn"), false, "a queue or an upgrade draws its own line")
})

test("the map lists what needs you first, then busy, idle, paused and away; the Hub's own and unpaired ones are not remote", () => {
  const vs = [
    view("local", { local: true }), view("idle-b"), view("away", { linked: false }), view("busy", { running: 3 }), view("idle-a"),
    view("paused", { paused: true }), view("gone", { state: "revoked" }), view("queued", { waiting: 1 }),
    view("failed", { version: "0.2.109", node: { upgrade: { policy: "notify", can: true, reason: "outdated", latest: run("rolled_back") } } }),
    view("upgrading", { node: { upgrade: { policy: "notify", can: true, reason: "outdated", latest: run("staging") } } }),
  ]
  assert.deepEqual(mapNodes(vs, "0.2.112").map((v) => v.id), ["failed", "queued", "upgrading", "busy", "idle-a", "idle-b", "paused", "away"])
  assert.equal(mapRank(view("x", { state: "pending" })), 1)
})

test("the order stays as it was while the page is open: new ones join at the end, gone ones leave", () => {
  assert.deepEqual(keepOrder(["a", "b", "c"], ["c", "a", "d", "b"]), ["a", "b", "c", "d"])
  assert.deepEqual(keepOrder(["a", "b", "c"], ["c", "a"]), ["a", "c"])
  assert.deepEqual(keepOrder([], ["x", "y"]), ["x", "y"])
})

test("the map's thresholds hold every count from no remote Node to more than twelve", () => {
  assert.equal(MAP_COMPACT_FROM, 7); assert.equal(MAP_MAX_NODES, 12); assert.equal(MAP_MAX_CLIENTS, 4)
  for (const n of [0, 1, 6, 7, 10, 12, 13, 30]) {
    const vs = Array.from({ length: n }, (_, i) => view("n" + String(i).padStart(2, "0")))
    const shown = mapNodes(vs).slice(0, MAP_MAX_NODES)
    assert.equal(shown.length, Math.min(n, MAP_MAX_NODES))
    assert.equal(n >= MAP_COMPACT_FROM, n > 6, `${n} Nodes: compact from seven`)
  }
})

test("clients: this one first, then online, then the latest, one per client, revoked ones left out, at most four", () => {
  const v = {
    tokens: [], current: "me", route: "lan", hub: { version: "0.2.112" },
    clients: [
      client("old", { last_seen: NOW - 40 * 86_400_000 }), client("phone", { kind: "ios", online: true, last_seen: NOW - 3_600_000 }),
      client("me", { last_seen: NOW - 120_000 }), client("cli", { kind: "cli", last_seen: NOW - 5_000 }),
      client("again", { name: "cli", kind: "cli", last_seen: NOW - 9_000 }), client("signed-out", { revoked: NOW }),
      client("tablet", { kind: "ios", name: "tablet", last_seen: NOW - 7_000 }),
    ],
  }
  const m = mapClients(v)
  assert.deepEqual(m.shown.map((c) => c.id), ["me", "phone", "cli", "tablet"])
  assert.equal(m.total, 5, "the same CLI signed in twice is one client")
  assert.equal(m.online, 2, "online: as the Hub says, and this one")
  assert.equal(clientOnline(client("x"), "x"), true)
  assert.equal(clientOnline(client("x", { online: false }), "x"), false, "the Hub's word wins")
  assert.deepEqual(mapClients(null), { shown: [], total: 0, online: 0 })
  const cookie = mapClients({ tokens: [], current: null, route: "lan", hub: { version: "x" }, clients: [client("phone", { online: false })] }, "Browser session")
  assert.deepEqual(cookie.shown.map((c) => [c.id, c.name, c.last_route]), [[SELF_CLIENT, "Browser session", "lan"], ["phone", "phone", "lan"]], "a cookie sign-in is still this browser")
  assert.equal(cookie.online, 1)
})

test("edges come from the cards' boxes: horizontal ends on a desktop, a trunk with branches on a phone", () => {
  assert.equal(hCurve(0, 0, 100, 50), "M0,0 C50,0 50,50 100,50")
  assert.equal(vCurve(0, 0, 40, 100), "M0,0 C0,50 40,50 40,100")
  const hub = { l: 300, t: 100, r: 500, b: 200 }, local = { l: 300, t: 240, r: 500, b: 300 }
  const w = wideEdges(hub, local, [{ id: "a", box: { l: 640, t: 60, r: 900, b: 100 } }], [{ id: "c", box: { l: 0, t: 130, r: 200, b: 170 } }])
  assert.deepEqual(w.edges.map((e) => e.id), ["local", "a"])
  assert.equal(w.edges[0].d, "M400,200 L400,240")
  assert.match(w.edges[1].d, /^M500,150 C570,150 570,80 640,80$/)
  assert.deepEqual(w.labels.find((l) => l.id === "a"), { id: "a", x: 606, y: 80 })
  assert.equal(w.access[0].d, "M200,150 C250,150 250,150 300,150")
  const frame = { l: 290, t: 80, r: 510, b: 320 }
  const n = narrowEdges(hub, frame, local, [{ id: "a", box: { l: 330, t: 360, r: 500, b: 400 } }, { id: "b", box: { l: 330, t: 420, r: 500, b: 460 } }], [{ id: "c", box: { l: 0, t: 0, r: 100, b: 40 } }])
  assert.equal(n.trunk, "M300,320 L300,432")
  assert.equal(n.edges.find((e) => e.id === "b").d, "M300,432 Q300,440 308,440 L330,440")
  assert.equal(n.access[0].d, "M50,40 C50,60 400,60 400,80")
  assert.equal(n.labels.length, 1, "only the Hub's own Node is labelled on a phone")
  assert.equal(narrowEdges(hub, frame, null, [], []).trunk, null)
})

test("a fingerprint is checked whole, group by group, however it was typed or pasted", () => {
  const fp = "9e3f5a07c21b84d60f9a77e3b10c4d52"
  assert.deepEqual(fpGroups(fp), ["9e3f", "5a07", "c21b", "84d6", "0f9a", "77e3", "b10c", "4d52"])
  assert.equal(fpDigits("fingerprint 9E3F 5A07-c21b"), "9e3f5a07c21b", "the word, spaces, dashes and case fall away")
  assert.equal(fpDigits("sha256:AB"), "ab")
  const all = fpCheck(fpGroups(fp), fp)
  assert.equal(all.all, true); assert.equal(all.wrong, 0)
  const two = fpCheck(["9e3f", "5a07"], fp)
  assert.equal(two.all, false, "two groups never confirm a Node")
  assert.deepEqual(two.groups.slice(0, 3), ["match", "match", "open"])
  const off = fpCheck([...fpGroups(fp).slice(0, 7), "4d53"], fp)
  assert.equal(off.all, false); assert.equal(off.wrong, 1); assert.equal(off.groups[7], "wrong")
  assert.equal(fpCheck([], "").all, false, "no fingerprint confirms nothing")
  assert.deepEqual(fpSpread([], "fingerprint " + fpGroups(fp).join(" "), 5, 8), fpGroups(fp), "a whole one fills from the first group")
  assert.deepEqual(fpSpread(["9e3f", "", "", ""], "5A07 C21B", 1, 4), ["9e3f", "5a07", "c21b", ""], "a part fills from where it was pasted")
  assert.deepEqual(fpSpread([], "01234567", 3, 4), ["", "", "", "0123"], "never past the last group")
})
