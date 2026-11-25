import assert from "node:assert/strict"
import { test } from "node:test"
import {
  attentionActionFor,
  effectiveDegradedReason,
  formatInstallCommand,
  harnessSection,
  harnessState,
  harnessSummary,
  partitionNodeHarnesses,
} from "../src/node-harness-model.ts"

const v = { linked: true }
const harness = (id, available = true, extra = {}) => ({
  id,
  name: id,
  available,
  version: "1.2",
  adapter_version: "3.4",
  models: [`${id}/model`],
  degraded_reason: null,
  installed: available === true,
  setup: null,
  error: null,
  ...extra,
})
const report = (hs = [], extra = {}) => ({
  state: "reported",
  checked_at: 1,
  attempted_at: 1,
  stale: false,
  refreshing: false,
  error: null,
  harnesses: hs,
  ...extra,
})

test("each summary uses only the supplied target report", () => {
  assert.equal(harnessSummary(v, report([harness("local-adapter")])), "可用：local-adapter 1.2")
  assert.equal(harnessSummary(v, report([harness("remote-adapter")])), "可用：remote-adapter 1.2")
  assert.match(harnessSummary(v, report()), /没有可用/)
  assert.match(harnessSummary(v, report([harness("absent", false)])), /没有可用/)
  assert.match(harnessSummary(v, report([harness("unprobed", null)])), /尚未探测/)
})

test("missing, unsupported, failed and offline reports are distinct from an empty success", () => {
  assert.match(harnessSummary(v), /尚未报告/)
  assert.match(harnessSummary(v, report([], { state: "unsupported" })), /不支持 harness 探测/)
  assert.match(harnessSummary(v, report([], { state: "failed", checked_at: null })), /探测失败.*重新/)
  assert.match(harnessSummary(v, report([harness("old")], { state: "failed" })), /探测失败.*上次/)
  assert.match(harnessSummary({ linked: false }), /离线.*尚无/)
  assert.match(harnessSummary({ linked: false }, report([harness("old")])), /离线.*过期/)
  assert.match(harnessSummary(v, report([harness("old")], { stale: true })), /过期.*old/)
})

test("installation and unknown availability have separate labels", () => {
  assert.equal(harnessState(harness("h", null)), "unknown")
  assert.equal(harnessState(harness("h", false, { installed: true, degraded_reason: "authentication_required" })), "authentication_required")
  assert.equal(harnessState(harness("h", false, { installed: true, degraded_reason: "command_not_in_service_path" })), "unavailable")
  assert.equal(harnessState(harness("h", false, { installed: false, setup: { state: "not_installed", components: [], can_install: false } })), "not_installed")
  assert.equal(harnessState(harness("h", false, { installed: true, setup: { state: "installing", components: [], can_install: false } })), "installing")
  assert.equal(harnessState(harness("h", false, { installed: true, degraded_reason: "initialize_failed" })), "unavailable")
  assert.equal(harnessState(harness("h")), "ready")
})

test("upgrade_only link explains version mismatch instead of probe failure", () => {
  const stale = { linked: true, linkMode: "upgrade_only", version: "0.9.0", hubVersion: "0.11.1", protocolOlder: "node" }
  assert.equal(
    harnessSummary(stale, report([], { state: "upgrade_required" })),
    "Node 版本 0.9.0 低于 Hub 0.11.1，升级后才能探测 harness",
  )
  assert.equal(
    harnessSummary(stale, report([], { state: "failed" })),
    "Node 版本 0.9.0 低于 Hub 0.11.1，升级后才能探测 harness",
  )
})

test("partitionNodeHarnesses separates available, needs-attention, and not-installed deterministically", () => {
  const item = (id, name, probe, update = null) => ({ id, name, probe, update, receipt: null })
  const partitioned = partitionNodeHarnesses([
    item("zebra", "Zebra CLI", harness("zebra", true)),
    item("alpha", "Alpha CLI", harness("alpha", true)),
    item("missing-cmd", "Missing Cmd", harness("missing-cmd", false, { installed: true, degraded_reason: "adapter_missing" })),
    item("path-mismatch", "Path Mismatch CLI", harness("path-mismatch", false, {
      installed: true,
      version: "",
      degraded_reason: "command_not_in_service_path",
      missing_command: {
        command: "agy",
        service_path: "/usr/bin:/bin",
        login_shell_path: "/Users/example/.local/bin/agy",
        login_shell: "/bin/zsh",
        refresh_command: "steve node refresh-env",
      },
    })),
    item("outdated", "Outdated CLI", harness("outdated", false, { installed: true, degraded_reason: "version_outdated" })),
    item("need-auth", "Auth CLI", harness("need-auth", false, { installed: true, degraded_reason: "authentication_required" })),
    item("absent-b", "Beta Absent", harness("absent-b", false, { installed: false, version: "" })),
    item("absent-a", "Alpha Installable", harness("absent-a", false, {
      installed: false,
      version: "",
      setup: { state: "not_installed", components: [], can_install: true },
    })),
  ])

  assert.deepEqual(partitioned.available.map((x) => x.id), ["alpha", "zebra"])
  // Needs-attention sorts by actionable severity: auth -> service PATH mismatch -> outdated -> adapter_missing
  assert.deepEqual(partitioned.attention.map((x) => x.id), ["need-auth", "path-mismatch", "outdated", "missing-cmd"])
  // Not-installed sorts installable first, then by name
  assert.deepEqual(partitioned.notInstalled.map((x) => x.id), ["absent-a", "absent-b"])

  assert.equal(harnessSection(partitioned.available[0]), "available")
  assert.equal(effectiveDegradedReason(partitioned.attention[0]), "authentication_required")
  assert.equal(effectiveDegradedReason(partitioned.attention[1]), "command_not_in_service_path")
  assert.equal(effectiveDegradedReason(partitioned.attention[2]), "version_outdated")
  assert.equal(effectiveDegradedReason(partitioned.notInstalled[0]), null)

  assert.equal(
    attentionActionFor(partitioned.attention[1], "command_not_in_service_path", { canInstallLocal: false, canUpgrade: false }),
    "copy_refresh_env",
  )
  assert.equal(
    attentionActionFor(partitioned.attention[2], "version_outdated", { canInstallLocal: true, canUpgrade: false }),
    "install",
  )
  assert.equal(
    attentionActionFor(partitioned.attention[0], "authentication_required", { canInstallLocal: true, canUpgrade: false }),
    "refresh",
  )
  assert.equal(formatInstallCommand("@google/gemini-cli"), "npm install -g @google/gemini-cli")
  assert.equal(
    formatInstallCommand("curl -fsSL https://antigravity.google/cli/install.sh | bash"),
    "curl -fsSL https://antigravity.google/cli/install.sh | bash",
  )
})
