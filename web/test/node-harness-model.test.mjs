import assert from "node:assert/strict"
import { test } from "node:test"
import { harnessState, harnessSummary } from "../src/node-harness-model.ts"

const v = { linked: true }
const harness = (id, available = true, extra = {}) => ({ id, name: id, available, version: "1.2", adapter_version: "3.4", models: [`${id}/model`], setup: null, error: null, ...extra })
const report = (hs = [], extra = {}) => ({ state: "reported", checked_at: 1, attempted_at: 1, stale: false, refreshing: false, error: null, harnesses: hs, ...extra })

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
  assert.equal(harnessState(harness("h", false, { degraded_reason: "authentication_required" })), "authentication_required")
  assert.equal(harnessState(harness("h", false, { degraded_reason: "command_not_in_service_path" })), "unavailable")
  assert.equal(harnessState(harness("h", false, { setup: { state: "not_installed" } })), "not_installed")
  assert.equal(harnessState(harness("h", false, { setup: { state: "installing" } })), "installing")
  assert.equal(harnessState(harness("h", false)), "unavailable")
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
