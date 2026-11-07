import assert from "node:assert/strict"
import { test } from "node:test"
import { checkStale, committedFailure, downloadPercent, enableOneClick, serviceCommands, stageIndex, stageLabel, upgradeGuide, upgradeStages, verifyUpgrade } from "../src/upgrade-guide.ts"

const home = "/Users/example/.steve", repo = "/Users/example/Agents/steve"

test("a local signing identity uses release packages and has an offline diagnostic entry", () => {
  const g = upgradeGuide({ kind: "local_release", home, release: "0.2.17-local-51826fc", repo }, "v0.2.20")
  const commands = g.steps.map(s => s.command).filter(Boolean)
  assert.deepEqual(commands, [], "a missing upgrade prerequisite must not send ordinary users to a compiler")
  assert.match(g.summary, /预编译发行包/)
  assert.match(g.backup, /\/Users\/example\/\.steve\/backups/)
  assert.match(g.recovery.join("\n"), /新版本和数据库会保留/)
  assert.deepEqual(serviceCommands("/opt/Steve data"), [
    "python3 '/opt/Steve data/current/ops/steve-service.py' hub status --home '/opt/Steve data'",
    "python3 '/opt/Steve data/current/ops/steve-service.py' hub doctor --home '/opt/Steve data'",
  ])
  assert.equal(g.steps.at(-1).title, "回到本页核对")
})

test("each other installation gets its own way, never another's script", () => {
  const script = upgradeGuide({ kind: "release_script", home, repo }, "0.2.20")
  assert.ok(script.steps.some(s => s.command === `zsh ${repo}/ops/release.sh main`))
  assert.match(script.recovery.join("\n"), /release\.sh --rollback/)
  const source = upgradeGuide({ kind: "source", home, repo }, "0.2.20")
  assert.match(source.steps[0].command, /sqlite3 .*\.backup .*before-0\.2\.20\.sqlite3/, "source runs back up before anything else")
  assert.ok(source.steps.some(s => s.command?.includes("git -C /Users/example/Agents/steve checkout v0.2.20")))
  const external = upgradeGuide({ kind: "external", home }, "0.2.20")
  assert.equal(external.steps.filter(s => s.command).length, 0, "nothing to run when Steve can't tell who owns the files")
  for (const g of [script, source, external]) assert.ok(!JSON.stringify(g).includes("local-release.py"))
})

test("progress follows the supervisor's journal and ends with this page's own check", () => {
  assert.equal(stageIndex("staging"), 0)
  assert.equal(stageIndex("backing_up"), 3)
  assert.equal(stageIndex("committed"), stageIndex("verifying"))
  assert.equal(stageIndex("succeeded"), upgradeStages.length - 1)
  assert.equal(stageIndex("rolled_back"), -1, "a failure is shown as recovery, not as a step")
})

test("upgraded only when the Hub, the served web, this page and health all agree", () => {
  const healthy = { ok: true, version: "0.2.20", web_version: "0.2.20", drain: false, recovery: { state: "ok" } }
  assert.equal(verifyUpgrade("v0.2.20", healthy, "0.2.20").done, true)
  const stale = verifyUpgrade("0.2.20", healthy, "0.2.17")
  assert.deepEqual([stale.done, stale.needsReload], [false, true], "an old page asks for a reload, it never claims success")
  const oldHub = verifyUpgrade("0.2.20", { ...healthy, version: "0.2.17" }, "0.2.20")
  assert.deepEqual([oldHub.done, oldHub.needsReload, oldHub.items.find(i => i.key === "hub").ok], [false, false, false])
  assert.equal(verifyUpgrade("0.2.20", { ...healthy, web_version: "0.2.17" }, "0.2.20").done, false, "a mixed pair is not upgraded")
  assert.equal(verifyUpgrade("0.2.20", { ...healthy, drain: true }, "0.2.20").done, false, "still in maintenance")
  assert.equal(verifyUpgrade("0.2.20", { ...healthy, recovery: { state: "failed" } }, "0.2.20").done, false)
  assert.equal(verifyUpgrade("0.2.20", { ...healthy, recovery: { state: "partial" } }, "0.2.20").done, true)
  const down = verifyUpgrade("0.2.20", null, "0.2.20")
  assert.deepEqual([down.done, down.items[0].detail], [false, "暂时连不上服务"])
})

test("only measured download bytes supply a percentage", () => {
  const p = (received_bytes, total_bytes, step = "downloading") => ({ step, download: { received_bytes, total_bytes } })
  assert.equal(downloadPercent(p(5, 10)), 50)
  assert.equal(downloadPercent(p(999, 1000)), 99, "rounding must not announce a finished download")
  assert.equal(downloadPercent(p(1000, 1000)), 100)
  for (const progress of [undefined, p(0, 0), p(20, 10), p(-1, 10), p(1, Infinity), p(10, 10, "signing")]) {
    assert.equal(downloadPercent(progress), undefined, "unknown progress is not a fabricated percent")
  }
  assert.equal(stageLabel("staging"), "下载并校验")
  assert.equal(stageLabel("staging", "release_bundle"), "下载并校验")
  assert.equal(stageLabel("staging", "local_release"), "本机构建（历史升级）")
  assert.equal(stageLabel("backingUp"), "备份数据库")
  assert.equal(enableOneClick("local_release"), undefined, "no Keychain migration asked of a local build any more")
  assert.match(enableOneClick("release_script"), /run-managed\.sh/)
})

test("the instance manager's page checks by itself when the last answer is missing or old, never during an upgrade", () => {
  const now = 10 * 3600_000
  const capability = { managed: false, can_execute: true, reason: "", owner: true }
  const check = (age) => ({ status: "up_to_date", checked_at: now - age, version: "0.2.0", release_url: null, notes: null, error: null })
  assert.equal(checkStale({ capability, check: null, operation: null }, now), true)
  assert.equal(checkStale({ capability, check: check(60_000), operation: null }, now), false)
  assert.equal(checkStale({ capability, check: check(2 * 3600_000), operation: null }, now), true)
  assert.equal(checkStale({ capability: { ...capability, owner: false }, check: null, operation: null }, now), false, "a check is the owner's to spend")
  assert.equal(checkStale({ capability: { ...capability, owner: undefined }, check: null, operation: null }, now), false, "an older Hub doesn't say")
  assert.equal(checkStale({ capability, check: null, operation: { phase: "staging" } }, now), false)
  assert.equal(checkStale({ capability, check: null, operation: { phase: "failed" } }, now), true)
})

test("a failure after the switch is recognised in either language", () => {
  assert.equal(committedFailure({ phase: "failed", error: "升级已提交：0.3.0 启动后没有正常起来" }), true)
  assert.equal(committedFailure({ phase: "failed", error: "Upgrade committed, but the new version didn't come up" }), true)
  assert.equal(committedFailure({ phase: "failed", error: "部署在切换前停止" }), false)
  assert.equal(committedFailure({ phase: "rolled_back", error: "升级已提交" }), false)
})
