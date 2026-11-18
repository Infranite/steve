import assert from "node:assert/strict"
import { test } from "node:test"
import { bytesLine, failureOf, formatBytes, formatElapsed, installTrail, resumable, transferDetails } from "../src/devices-install.ts"

const i18n = (await import("../src/i18n/index.ts")).default
const tIn = (lng) => (key, o) => i18n.t(key, { lng, ...o })
const t = tIn("zh-CN")
const MB = 1024 * 1024
const op = (o) => ({ id: "ni_1", state: "running", command: "tailcat serve …", steps: [], node_id: null, error: null, failed_step: null, address: "tcX", ...o })
const step = (kind, o) => ({ kind, at: 0, text: `${kind} text`, ended: 1, elapsed_ms: 2000, progress: null, ...o })

test("bytes and times read short", () => {
  assert.equal(formatBytes(512), "512 B")
  assert.equal(formatBytes(1.5 * 1024), "1.5 KB")
  assert.equal(formatBytes(12.34 * MB), "12.3 MB")
  assert.equal(formatBytes(140 * MB), "140 MB")
  assert.equal(formatElapsed(4200), "4 s")
  assert.equal(formatElapsed(65_000), "1:05")
  assert.equal(formatElapsed(3_723_000), "1:02:03")
  assert.equal(bytesLine({ phase: "download", done: 12.3 * MB, total: 40 * MB, rate: 2.1 * MB }, t), "12.3 MB / 40.0 MB · 2.1 MB/s")
  assert.equal(bytesLine({ phase: "send", done: 3 * MB, total: null, rate: null }, t), "3.0 MB")
})

test("a download in progress shows its bytes, speed, share and time, the steps after it waiting", () => {
  const now = 1_000_000
  const trail = installTrail(op({ steps: [
    step("probe"),
    step("fetch", { ended: null, elapsed_ms: 9000, progress: { phase: "download", done: 10 * MB, total: 40 * MB, rate: MB } }),
  ] }), now, t)
  assert.deepEqual(trail.map(s => [s.id, s.state]), [["probe", "done"], ["fetch", "running"], ["send", "pending"], ["setup", "pending"]])
  assert.equal(trail[0].duration, 2000)
  const f = trail[1]
  assert.equal(f.title, "fetch text", "the Hub's line while it runs")
  assert.equal(trail[0].title, "连接目标机器")
  assert.equal(f.status, "10.0 MB / 40.0 MB · 1.0 MB/s")
  assert.equal(f.percent, 25)
  assert.equal(f.startedAt, now - 9000)
  assert.deepEqual(f.substeps.map(s => [s.title, s.state]), [["下载", "running"], ["校验", "pending"]])
  assert.equal(trail[2].title, "把安装包发到目标机器")
})

test("checking, a cached copy, sending and the setup phases each say what they do", () => {
  const verify = installTrail(op({ steps: [step("probe"), step("fetch", { ended: null, progress: { phase: "verify", done: 40, total: 40, rate: null } })] }), 0, t)[1]
  assert.equal(verify.status, "校验中")
  assert.equal(verify.percent, undefined)
  assert.deepEqual(verify.substeps.map(s => s.state), ["done", "running"])
  const cached = installTrail(op({ steps: [step("probe"), step("fetch", { ended: null, progress: { phase: "cached", done: 40, total: 40, rate: null } })] }), 0, t)[1]
  assert.equal(cached.status, "使用之前下载的安装包，重新校验")
  assert.equal(cached.substeps.length, 1)
  const send = installTrail(op({ steps: [step("probe"), step("fetch"), step("send", { ended: null, progress: { phase: "send", done: 30 * MB, total: 40 * MB, rate: 512 * 1024 } })] }), 0, t)[2]
  assert.equal(send.status, "30.0 MB / 40.0 MB · 512 KB/s")
  assert.equal(send.percent, 75)
  const setup = installTrail(op({ steps: [step("probe"), step("fetch"), step("send"), step("setup", { ended: null, progress: { phase: "pair", done: 0, total: null, rate: null } })] }), 0, t)[3]
  assert.equal(setup.status, "配对")
  assert.deepEqual(setup.substeps.map(s => [s.id, s.state]), [["install", "done"], ["pair", "running"], ["adapters", "pending"], ["start", "pending"]])
})

test("a finished install has every step done with its time; the Hub's last line is not a step", () => {
  const trail = installTrail(op({ state: "done", node_id: "n1", steps: [step("probe"), step("fetch"), step("send"), step("setup", { progress: { phase: "start", done: 0, total: null, rate: null } }), step("done", { elapsed_ms: 0 })] }), 0, t)
  assert.deepEqual(trail.map(s => s.state), ["done", "done", "done", "done"])
  assert.ok(trail.every(s => s.duration === 2000))
  assert.ok(trail[3].substeps.every(s => s.state === "done"))
})

test("a failure says at which step, after how long and why", () => {
  const o = op({ state: "failed", failed_step: "send", error: "发送安装包卡住了", steps: [step("probe"), step("fetch"), step("send", { elapsed_ms: 74_000, text: "正在把安装包发到目标机器" })] })
  const trail = installTrail(o, 0, t)
  assert.deepEqual(trail.map(s => s.state), ["done", "done", "failed", "pending"])
  assert.deepEqual(failureOf(o, t), { step: "把安装包发到目标机器", elapsed: "1:14", error: "发送安装包卡住了" })
  assert.equal(trail[2].title, "把安装包发到目标机器", "a step that ended goes by its name")
  assert.equal(failureOf(op({}), t), null)
  const said = t("shell:devices.install.failedAt", { step: "x", elapsed: "1:14" })
  assert.ok(said.includes("x") && said.includes("1:14"), said)
})

test("the dialog picks up the running install, else the newest unfinished one", () => {
  assert.equal(resumable([op({ id: "a", state: "failed" }), op({ id: "b" })]).id, "b")
  assert.equal(resumable([op({ id: "a", state: "failed" }), op({ id: "b", state: "done" })]).id, "a")
  assert.equal(resumable([op({ id: "a", state: "done" })]), null, "a finished one is not picked up again")
  assert.equal(resumable([]), null)
})

test("every install string is there in both languages", () => {
  for (const lang of ["zh-CN", "en"]) {
    for (const key of ["progress", "working", "failed", "finished", "pending", "of", "failedAt", "joined", "retry", "cancel", "cancelHint",
      "kind.probe", "kind.fetch", "kind.send", "kind.setup",
      "phase.download", "phase.verify", "phase.verifying", "phase.cached", "phase.install", "phase.pair", "phase.adapters", "phase.start"])
      assert.ok(i18n.exists(`shell:devices.install.${key}`, { lng: lang }), `${lang} ${key}`)
  }
  assert.equal(installTrail(op({ steps: [] }), 0, tIn("en"))[0].title, "Reach the target machine")
})

test("transfer details use observed route and ETA without equating buffered bytes with receipt", () => {
  const partial = { phase: "send", done: 30 * MB, total: 40 * MB, rate: 512 * 1024, eta_secs: 20 }
  const sending = (progress, route = "derp_observed") => op({
    transfer_route: route, steps: [step("probe"), step("fetch"), step("send", { ended: null, progress })],
  })
  assert.deepEqual(transferDetails(sending(partial), t), {
    route: "tailcat SSH · 观测到 DERP 中继",
    bytes: "30.0 MB / 40.0 MB", rate: "512 KB/s", eta: "预计还需 20 s 传完",
    transferTime: undefined, unpackTime: undefined,
  })
  assert.equal(transferDetails(sending({ ...partial, rate: null }, null), t).eta, "正在估算传输剩余时间…")
  assert.equal(transferDetails(sending(partial, null), t).route, "tailcat SSH · 数据路径未确认")
  const queued = sending({ ...partial, done: partial.total })
  assert.equal(transferDetails(queued, t).eta, "等待目标机确认收包…")
  assert.equal(installTrail(queued, 0, t)[2].percent, 99)
  const unpacking = sending({ ...partial, phase: "unpack", done: partial.total, rate: null }, "direct_observed")
  Object.assign(unpacking.steps[2], { transfer_ms: 2700, unpack_ms: 400 })
  assert.deepEqual(transferDetails(unpacking, t), {
    route: "tailcat SSH · 观测到直连",
    bytes: undefined, rate: undefined, eta: undefined,
    transferTime: "传输：3 s", unpackTime: "目标机解压：<1 秒",
  })
  assert.equal(installTrail(unpacking, 0, t)[2].status, "在目标机器上解压")
})
