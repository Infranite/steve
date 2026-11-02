import test, { after } from "node:test"
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { reportedConfig, requireEffort } from "../src/effort.js"
import type { TurnEnd } from "../src/acp.js"

const home = mkdtempSync(join(tmpdir(), "steve-effort-"))
Object.assign(process.env, { STEVE_HOME: home, STEVE_REPO: join(home, "no-repo"), STEVE_ADAPTER_CLAUDE: join(import.meta.dirname, "fake-effort-acp.js") })
const { ensureWorkspace, WS, bots, sessions } = await import("../src/repo.js")
const { db } = await import("../src/db.js")
const acp = await import("../src/acp.js")
ensureWorkspace()
after(() => { acp.shutdown(); db.close(); setImmediate(() => { rmSync(home, { recursive: true, force: true }); process.exit(0) }) })

test("ACP reasoning selection follows the adapter's advertised control and supported values", () => {
  const options = [
    { id: "model", currentValue: "actual-model" },
    { id: "thinking", category: "thought_level", currentValue: "high", options: [{ group: "levels", options: [{ value: "low" }, { value: "high" }] }] },
  ]
  assert.equal(requireEffort(options, "high"), "thinking")
  assert.deepEqual(reportedConfig(options), { model: "actual-model", reasoning_effort: "high" })
  assert.throws(() => requireEffort(options, "xhigh"), /不支持思考强度 xhigh/)
  assert.throws(() => requireEffort([], "high"), /不支持指定思考强度/)
})

test("adapter defaults are reported without inventing reasoning or context usage", () => {
  assert.deepEqual(reportedConfig([{ id: "effort", currentValue: "default" }]), { reasoning_effort: null })
  assert.deepEqual(reportedConfig([]), {})
})

test("bot reasoning is persisted, snapshotted per session, and can return to the default", () => {
  const bot = bots.create(WS, { name: "思考测试", harness: "codex", reasoning_effort: "high" }, "test")
  assert.equal(bot.reasoning_effort, "high")
  const session = sessions.create({ workspace_id: WS, item_id: null, bot_id: bot.id, harness: bot.harness, model: null, reasoning_effort: bot.reasoning_effort, cwd: home, title: "快照", token: "test" })
  bots.patch(bot.id, { reasoning_effort: null }, "test")
  assert.equal(bots.get(bot.id).reasoning_effort, null)
  assert.equal(sessions.get(session.id)?.reasoning_effort, "high")
  sessions.patch(session.id, reportedConfig([{ id: "reasoning_effort", currentValue: "medium" }]))
  assert.equal(sessions.get(session.id)?.reasoning_effort, "medium")
})

test("runner applies effort after model switching, persists actual configuration and context, and reapplies on resume", async () => {
  const session = sessions.create({ workspace_id: WS, item_id: null, bot_id: null, harness: "claude-code", model: "requested-model-b", reasoning_effort: "high", cwd: home, title: "ACP", token: "acp" })
  const turn = () => new Promise<TurnEnd>((resolve) => { acp.setTurnEndHandler(resolve); void acp.prompt(session.id, "test", "me") })
  for (let n = 0; n < 2; n++) {
    const result = await turn()
    assert.equal(result.stopReason, "end_turn", result.error)
    const actual = sessions.get(session.id)!
    assert.equal(actual.model, "actual-model-b")
    assert.equal(actual.reasoning_effort, "high")
    assert.equal(actual.used, 4200)
    assert.equal(actual.size, 20000)
    acp.release(session.id)
  }
})

test("unsupported explicit reasoning fails before the adapter receives a prompt", async () => {
  const session = sessions.create({ workspace_id: WS, item_id: null, bot_id: null, harness: "claude-code", model: "requested-model-b", reasoning_effort: "xhigh", cwd: home, title: "unsupported", token: "unsupported" })
  const result = await new Promise<TurnEnd>((resolve) => { acp.setTurnEndHandler(resolve); void acp.prompt(session.id, "test", "me") })
  assert.equal(result.stopReason, "error")
  assert.match(result.error ?? "", /不支持思考强度 xhigh/)
})

test("adapter effort substitutions do not silently run with another intensity", async () => {
  const session = sessions.create({ workspace_id: WS, item_id: null, bot_id: null, harness: "claude-code", model: "requested-substitute", reasoning_effort: "high", cwd: home, title: "substitute", token: "substitute" })
  const result = await new Promise<TurnEnd>((resolve) => { acp.setTurnEndHandler(resolve); void acp.prompt(session.id, "test", "me") })
  assert.equal(result.stopReason, "error")
  assert.match(result.error ?? "", /思考强度未生效/)
  assert.equal(sessions.get(session.id)?.reasoning_effort, "low", "the rejected adapter's actual state remains visible")
})
