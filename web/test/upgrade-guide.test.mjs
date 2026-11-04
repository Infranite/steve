import assert from "node:assert/strict"
import { test } from "node:test"
import { stageIndex, upgradeGuide, upgradeStages, verifyUpgrade } from "../src/upgrade-guide.ts"

const home = "/Users/me/.steve", repo = "/Users/me/Agents/steve"

test("a signed local build is told to build the Release tag here and deploy it, with this machine's paths", () => {
  const g = upgradeGuide({ kind: "local_release", home, release: "0.2.17-local-51826fc", repo }, "v0.2.20")
  const commands = g.steps.map(s => s.command).filter(Boolean)
  assert.deepEqual(commands, [
    `git -C ${repo} fetch --tags origin && git -C ${repo} rev-parse --verify v0.2.20^{commit}`,
    `python3 ${repo}/ops/local-release.py build v0.2.20`,
    `python3 ${repo}/ops/local-release.py deploy ${home}/releases/0.2.20-local-*`,
  ])
  assert.match(g.backup, /\/Users\/me\/\.steve\/backups/)
  assert.match(g.recovery.join("\n"), /deploy \/Users\/me\/\.steve\/releases\/0\.2\.17-local-51826fc/, "the way back names the release running now")
  assert.equal(g.steps.at(-1).title, "回到本页核对")
})

test("each other installation gets its own way, never another's script", () => {
  const script = upgradeGuide({ kind: "release_script", home, repo }, "0.2.20")
  assert.ok(script.steps.some(s => s.command === `zsh ${repo}/ops/release.sh main`))
  assert.match(script.recovery.join("\n"), /release\.sh --rollback/)
  const source = upgradeGuide({ kind: "source", home, repo }, "0.2.20")
  assert.match(source.steps[0].command, /sqlite3 .*\.backup .*before-0\.2\.20\.sqlite3/, "source runs back up before anything else")
  assert.ok(source.steps.some(s => s.command?.includes("git -C /Users/me/Agents/steve checkout v0.2.20")))
  const external = upgradeGuide({ kind: "external", home }, "0.2.20")
  assert.equal(external.steps.filter(s => s.command).length, 0, "nothing to run when Steve can't tell who owns the files")
  for (const g of [script, source, external]) assert.ok(!JSON.stringify(g).includes("local-release.py"))
  // without a checkout on this machine the command says where to fill it in; paths with spaces are quoted
  assert.match(upgradeGuide({ kind: "local_release", home: "/Users/a b/.steve" }, "0.2.20").steps[1].command, /^python3 <Steve 源码目录>\/ops/)
  assert.match(upgradeGuide({ kind: "local_release", home: "/Users/a b/.steve", repo }, "0.2.20").steps[2].command, /deploy '\/Users\/a b\/\.steve'\/releases\/0\.2\.20-local-\*$/)
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
