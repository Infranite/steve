import assert from "node:assert/strict"
import { test } from "node:test"
import {
  NODE_TABS,
  parseNodeTab,
  nodeHref,
  nodeActionItems,
  nodeTabBadges,
  nodeDiagnosticSnapshot,
} from "../src/node-detail-model.ts"

const NOW = Date.UTC(2026, 10, 25, 10, 0)
const view = (extra = {}) => ({
  id: "n-1",
  name: "Design Mac",
  local: false,
  state: "active",
  linked: true,
  paused: false,
  running: 1,
  waiting: 0,
  max: 6,
  version: "0.2.112",
  fingerprint: "9e3f5a07c21b84d60f9a77e3b10c4d52",
  lastSeen: NOW - 15_000,
  ...extra,
})

test("parseNodeTab accepts known tabs and falls back to overview; nodeHref omits overview", () => {
  for (const tab of NODE_TABS) assert.equal(parseNodeTab(tab), tab)
  assert.equal(parseNodeTab(undefined), "overview")
  assert.equal(parseNodeTab(""), "overview")
  assert.equal(parseNodeTab("unknown-tab"), "overview")
  assert.equal(nodeHref("n-1"), "#/devices/nodes/n-1")
  assert.equal(nodeHref("n-1", "overview"), "#/devices/nodes/n-1")
  assert.equal(nodeHref("n-1", "harness"), "#/devices/nodes/n-1/harness")
  assert.equal(nodeHref("a/b", "diagnostics"), "#/devices/nodes/a%2Fb/diagnostics")
})

test("nodeActionItems surfaces offline, file access, harness auth, upgrades and waiting sessions in severity order", () => {
  const v = view({
    version: "0.2.109",
    node: {
      node_id: "n-1",
      name: "Design Mac",
      fingerprint: "9e3f5a07c21b84d60f9a77e3b10c4d52",
      state: "active",
      linked: true,
      local: false,
      host: {
        os: "macos",
        arch: "aarch64",
        file_access: { status: "missing", locations: [{ location: "documents", status: "missing" }] },
      },
      upgrade: {
        build: { version: "0.2.109", commit: "abc", target: "aarch64-apple-darwin" },
        policy: "notify",
        can: true,
        reason: "outdated",
        latest: {
          operation_id: "op1",
          version: "0.2.112",
          from_version: "0.2.109",
          phase: "rolled_back",
          error: "timeout",
          requested_by: "owner",
          force: false,
          created: NOW - 60_000,
          updated: NOW - 30_000,
        },
      },
    },
  })
  const harnessReport = {
    state: "reported",
    checked_at: NOW,
    attempted_at: NOW,
    stale: false,
    refreshing: false,
    error: null,
    harnesses: [
      { id: "h1", name: "Fixture CLI", available: false, version: "1.0", adapter_version: "1.0", degraded_reason: "authentication_required", models: [], error: null, setup: null },
    ],
  }
  const items = nodeActionItems(v, {
    hubVersion: "0.2.112",
    harnessReport,
    attentionCount: 2,
    canUpgrade: true,
  })
  assert.deepEqual(
    items.map((i) => [i.kind, i.tone, i.tab, i.command]),
    [
      ["upgradeFailed", "bad", "version", "startUpgrade"],
      ["fileAccess", "warn", "access", "recheckFileAccess"],
      ["harnessAuth", "warn", "harness", "refreshHarnesses"],
      ["sessionsWaiting", "warn", "work", "openTab"],
    ],
  )
  const badges = nodeTabBadges(v, items, { activeSessions: 3, harnessReadyCount: 1 })
  assert.deepEqual(badges.version, { count: 1, tone: "bad" })
  assert.deepEqual(badges.access, { count: 1, tone: "warn" })
  assert.deepEqual(badges.harness, { count: 1, tone: "warn" })
  assert.deepEqual(badges.work, { count: 2, tone: "warn" })
  assert.deepEqual(badges.overview, {})
})

test("nodeActionItems handles offline, upgrading, pending and revoked cleanly", () => {
  const off = view({
    linked: false,
    node: {
      node_id: "n-off",
      name: "iMac",
      fingerprint: "abcd",
      state: "active",
      linked: false,
      local: false,
      offline: { reason: "unreachable", since: NOW - 3_600_000, duration_ms: 3_600_000 },
    },
  })
  const offItems = nodeActionItems(off, { hubVersion: "0.2.112" })
  assert.equal(offItems.length, 1)
  assert.equal(offItems[0].kind, "offline")
  assert.equal(offItems[0].tab, "diagnostics")

  const upgrading = view({
    running: 2,
    node: {
      node_id: "n-up",
      name: "Builder",
      fingerprint: "abcd",
      state: "active",
      linked: true,
      local: false,
      running: 2,
      upgrade: {
        policy: "notify",
        can: false,
        reason: "busy",
        latest: { operation_id: "op2", version: "0.2.112", from_version: "0.2.109", phase: "draining", requested_by: "owner", force: false, created: NOW, updated: NOW },
      },
    },
  })
  const upItems = nodeActionItems(upgrading, { hubVersion: "0.2.112", canUpgrade: true })
  assert.deepEqual(upItems.map((i) => [i.kind, i.command, i.tone]), [["upgradeRunning", "switchUpgradeNow", "info"]])

  assert.deepEqual(nodeActionItems(view({ state: "revoked", linked: false })), [])
})

test("nodeDiagnosticSnapshot produces sanitized read-only state matching vitals and harness summaries", () => {
  const snap = nodeDiagnosticSnapshot(view(), {
    hubVersion: "0.2.112",
    now: NOW,
    vitals: {
      latest: { at: NOW, sampled_at: NOW, cpus: 8, load: [1, 1, 1], mem_total: 16 * 2 ** 30, mem_available: 8 * 2 ** 30, roots: [], sessions: 1, rtt_ms: 12 },
      series: [],
      gaps: [{ from: NOW - 120_000, to: NOW - 60_000 }],
      sampling: { interval_ms: 60_000, stale_after_ms: 180_000, now: NOW },
      tokens: { per_minute: 500, today: 12_000 },
    },
  })
  assert.equal(snap.captured_at, new Date(NOW).toISOString())
  assert.equal(snap.node.id, "n-1")
  assert.equal(snap.vitals?.gaps_count, 1)
  assert.equal(snap.vitals?.tokens.today, 12_000)
})
