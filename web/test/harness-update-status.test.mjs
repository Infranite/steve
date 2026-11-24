import assert from "node:assert/strict"
import { test } from "node:test"
import {
  availableUpgradableHarnesses,
  effectiveAutoUpgrade,
  hasUpgradeAction,
  isAutoUpgradeOverridden,
  receiptStatus,
  selfUpdateTargets,
  supportsAutoUpgrade,
  updateTargets,
} from "../src/harness-update-status.ts"

const comp = (overrides = {}) => ({
  id: "cli",
  name: "Fixture CLI",
  managed: true,
  source: "steve_managed",
  upgrade_method: "steve_managed",
  can_upgrade: true,
  state: "current",
  installed: "1.0.0",
  ...overrides,
})

test("only a final successful receipt reports success and waiting stays distinct from running", () => {
  assert.equal(receiptStatus(null), null)
  for (const state of ["accepted", "running"]) {
    assert.equal(receiptStatus({ operation: "op", state: { state } }), "running")
  }
  assert.equal(receiptStatus({ operation: "op", state: { state: "waiting" } }), "waiting")
  assert.equal(receiptStatus({ operation: "op", state: { state: "unknown", reason: "connection lost" } }), "unknown")
  for (const outcome of ["failed", "cancelled", "timed_out", "interrupted"]) {
    assert.equal(receiptStatus({ operation: "op", state: { state: "finished", outcome } }), "failed")
  }
  assert.equal(receiptStatus({ operation: "op", state: { state: "finished", outcome: "succeeded" } }), "succeeded")
})

test("one click targets managed and verified external components with a checked update or self-updater", () => {
  const harness = {
    can_upgrade: true,
    components: [
      comp({ id: "cli_brew", managed: false, source: "homebrew_cask", upgrade_method: "homebrew_cask", can_upgrade: true, state: "available", latest: "3.0.0" }),
      comp({ id: "cli_ext_ro", managed: false, source: "external", upgrade_method: "unsupported", can_upgrade: false, state: "available", latest: "3.0.0" }),
      comp({ id: "adapter", managed: true, source: "steve_managed", upgrade_method: "steve_managed", can_upgrade: true, state: "available", latest: "2.0.0" }),
      comp({ id: "self_cli", managed: false, source: "standalone", upgrade_method: "self_update", can_upgrade: true, state: "check_on_upgrade", latest: undefined }),
      comp({ id: "old", state: "current", latest: "1.0.0" }),
      comp({ id: "failed", state: "check_failed", latest: "9.0.0" }),
      comp({ id: "missing", can_upgrade: false, state: "not_installed", latest: "2.0.0" }),
    ],
  }
  assert.deepEqual(updateTargets(harness), { cli_brew: "3.0.0", adapter: "2.0.0" })
  assert.deepEqual(selfUpdateTargets(harness), ["self_cli"])
  assert.equal(hasUpgradeAction(harness), true)
  assert.equal(hasUpgradeAction({ can_upgrade: true, components: [comp({ id: "ro", can_upgrade: false, state: "unsupported" })] }), false)
})

test("auto-upgrade policy resolves Node default, per-harness overrides, and check-on-upgrade capability", () => {
  const policy = { enabled: false, harnesses: { alpha: true, beta: false } }
  assert.equal(effectiveAutoUpgrade(null, "alpha"), false)
  assert.equal(effectiveAutoUpgrade(policy, "alpha"), true)
  assert.equal(effectiveAutoUpgrade(policy, "beta"), false)
  assert.equal(effectiveAutoUpgrade(policy, "gamma"), false)
  assert.equal(isAutoUpgradeOverridden(policy, "alpha"), true)
  assert.equal(isAutoUpgradeOverridden(policy, "gamma"), false)

  const nodeOn = { enabled: true, harnesses: { beta: false } }
  assert.equal(effectiveAutoUpgrade(nodeOn, "gamma"), true)
  assert.equal(effectiveAutoUpgrade(nodeOn, "beta"), false)

  assert.equal(supportsAutoUpgrade({ components: [comp({ source: "homebrew_cask", upgrade_method: "homebrew_cask", can_upgrade: true, state: "current" })] }), true)
  assert.equal(supportsAutoUpgrade({ components: [comp({ source: "standalone", upgrade_method: "self_update", can_upgrade: true, state: "check_on_upgrade" })] }), false)
  assert.equal(supportsAutoUpgrade({ components: [comp({ source: "external", upgrade_method: "unsupported", can_upgrade: false, state: "unsupported" })] }), false)
})

test("availableUpgradableHarnesses lists idle or queued-eligible updates and excludes active/unknown runs", () => {
  const mk = (harness, state, activeSessions = 0) => ({
    harness,
    name: harness,
    can_upgrade: true,
    active_sessions: activeSessions,
    components: [comp({ id: "cli", managed: false, source: "homebrew_cask", upgrade_method: "homebrew_cask", can_upgrade: true, state, latest: state === "available" ? "2.0.0" : undefined })],
  })
  const data = {
    report: {
      node: "n-1",
      checking: false,
      harnesses: [
        mk("ready", "available", 0),
        mk("busy_running", "available", 0),
        mk("busy_waiting", "available", 2),
        mk("self_only", "check_on_upgrade", 0),
      ],
    },
    at: 1,
    runs: {
      busy_running: { operation: "op1", state: { state: "running" } },
      busy_waiting: { operation: "op2", state: { state: "waiting" } },
    },
    retryable_unknown: [],
    auto_upgrade: { enabled: false, harnesses: {} },
    error: null,
    code: null,
  }
  assert.deepEqual(availableUpgradableHarnesses(data).map((h) => h.harness), ["ready"])
})
